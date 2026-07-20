//! `git range-diff` — compare two versions of a patch series.
//!
//! A port of upstream `range-diff.c`, `builtin/range-diff.c` and
//! `linear-assignment.c` on top of the vendored gitoxide. The pipeline is
//! reproduced stage for stage:
//!
//! 1. `read_patches()` — walk each commit range (merges excluded, oldest first)
//!    and render every commit into the *canonical patch text* upstream builds by
//!    post-processing `git log --no-color -p --no-merges --reverse --date-order
//!    --no-prefix --pretty=medium` output: a ` ## Metadata ##` block holding the
//!    (mailmap-resolved) `Author:` line, a ` ## Commit message ##` block holding
//!    the 4-space-indented, right-trimmed message, and one ` ## <path> ##`
//!    section per changed file whose hunk headers are rewritten to
//!    `@@ <path>: <function>` — so hunk line numbers never enter the comparison.
//!    Because upstream feeds the `diff --git` header block through
//!    `parse_git_diff_header()`, the `index`/`--- `/`+++ `/`new file mode` lines
//!    are consumed rather than kept: abbreviated blob ids are irrelevant to the
//!    output, and this port does not compute them.
//! 2. `find_exact_matches()` — hash the diff portion of every left patch and
//!    pair off byte-identical right patches. Upstream's hashmap chains are LIFO,
//!    so duplicate left patches match highest-index-first; reproduced.
//! 3. `get_correspondences()` — build the `n x n` cost matrix from `diffsize()`
//!    (a 3-context diff-of-diffs *without* the indent heuristic, counting hunks
//!    plus lines), pad it with `diffsize * creation_factor / 100` create/delete
//!    entries, and solve it with `compute_assignment()`, a direct port of
//!    `linear-assignment.c` (Jonker–Volgenant shortest augmenting path).
//! 4. `output()` — emit the `1:  abc123 ! 2:  def456 <subject>` pair headers and,
//!    for each matched pair, the diff-of-diffs indented by four spaces, with no
//!    file headers (`suppress_diff_headers`) and the hunk header reduced to `@@`
//!    plus a section name (`suppress_hunk_header_line_count`). The section name
//!    comes from upstream's `section_headers` userdiff driver — the two patterns
//!    `^ ## (.*) ##$` and `^.?@@ (.*)$` — ported by hand together with
//!    `ff_regexp()`'s 80-byte cap and trailing-whitespace trim, the backwards
//!    search bounded by the previous hunk, and `xdl_emit_diff()`'s quirk that a
//!    hunk with no match repeats the previous hunk's section name.
//!
//! ### Covered (stdout byte-identical to stock git, exit code included)
//!
//! * `range-diff <range1> <range2>`, `range-diff <rev1>...<rev2>` and
//!   `range-diff <base> <rev1> <rev2>`, dispatched with upstream's precedence
//!   (three committishes first, then two ranges, then one symmetric range).
//! * Ranges spelled `<a>..<b>` or `<a>...<b>`, either side defaulting to `HEAD`
//!   when empty.
//! * `--creation-factor=<n>`, `--left-only`, `--right-only`, `--no-dual-color`
//!   and `--no-color`. Dual and simple coloring are byte-identical once color is
//!   off, which is the only mode this port emits.
//! * `--left-only` together with `--right-only`: upstream's `error()` on stderr
//!   and its exit status.
//!
//! ### Not covered — these `bail!` rather than emit output that would diverge
//!
//! * Color in any form: `--color`, `--color=<when>`, and `--dual-color` (which
//!   upstream uses to *force* color on). The dual-color markup is not ported.
//! * `--notes[=<ref>]` / `--no-notes`, and repositories carrying a `refs/notes/
//!   commits` ref — upstream asks `git log` to show notes by default, so a note
//!   would silently change the compared text.
//! * `--diff-merges=<format>` / `--remerge-diff` (merges are ignored here, which
//!   is the default upstream behaviour), pathspec limiting (`[--] <path>...`),
//!   and every other `git diff` option upstream forwards to the inner patches.
//! * Commits containing a rename that git's `diffcore-rename` would detect.
//!   These are found by re-running the tree diff with gitoxide's rename tracker
//!   at git's default 50% threshold, and refused: upstream's `old => new`
//!   section header depends on `diffcore-delta` similarity scoring and on
//!   rename-aware diff-queue ordering, neither of which is ported.
//! * `-h`: upstream's usage text concatenates the entire `git diff` option list,
//!   which is not ported.
//!
//! ### Known deviations, stated rather than hidden
//!
//! * Upstream orders each range with `--date-order`, i.e. commit-date order
//!   constrained by topology. This port implements the topological constraint
//!   exactly (Kahn's algorithm over in-range child counts, newest commit date
//!   first), which is identical for the linear patch series range-diff exists to
//!   compare, but may break commit-date ties differently on merge-heavy ranges,
//!   because upstream's tie-break is its binary heap's internal order.
//! * A usage error prints `fatal: <reason>` on stderr and exits 129 like
//!   upstream, but without the option list upstream prints after it. Stdout is
//!   empty either way.

use anyhow::{anyhow, bail, Result};
use std::collections::{BinaryHeap, HashMap};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::BStr;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, Diff, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::prelude::ObjectIdExt;

/// `RANGE_DIFF_CREATION_FACTOR_DEFAULT`.
const CREATION_FACTOR_DEFAULT: i64 = 60;
/// `COST_MAX` from `linear-assignment.h`, the cost cap that prevents overflow.
const COST_MAX: i64 = 1 << 16;
/// `sizeof(struct func_line.buf)` — the hard cap on a hunk header's section name.
const FUNC_BUF_SIZE: usize = 80;
/// `FIRST_FEW_BYTES` — how far `buffer_is_binary()` looks for a NUL byte.
const FIRST_FEW_BYTES: usize = 8000;
/// The four-space `output_prefix` upstream installs for the diff-of-diffs.
const INDENT: &[u8] = b"    ";

/// One commit rendered into its canonical patch text: upstream's
/// `struct patch_util` fused with the `string_list` item holding the text.
struct Patch {
    /// Position within its range, upstream's `util->i`.
    index: usize,
    /// `find_unique_abbrev()` of the commit id, for the pair header.
    abbrev: String,
    /// One-line subject (`CMIT_FMT_ONELINE`), for the pair header.
    subject: Vec<u8>,
    /// The full patch: metadata, message, and every file section.
    text: Vec<u8>,
    /// Offset of the first ` ## <path> ##` section. Left at 0 for a commit with
    /// no diff, exactly as upstream leaves `diff_offset` zeroed there, so that
    /// `diff()` then covers the whole patch.
    diff_offset: usize,
    /// Number of diff lines, upstream's `diffsize`, used for the creation cost.
    diffsize: i64,
    /// Index of the corresponding patch in the other range, or -1.
    matching: i64,
    /// Whether this left-hand patch has already been printed.
    shown: bool,
}

impl Patch {
    /// Upstream's `util->diff`: the patch text from the first file section on.
    fn diff(&self) -> &[u8] {
        &self.text[self.diff_offset..]
    }
}

/// Parsed command line.
struct Opts {
    creation_factor: i64,
    left_only: bool,
    right_only: bool,
}

pub fn range_diff(args: &[String]) -> Result<ExitCode> {
    let mut opts = Opts {
        creation_factor: CREATION_FACTOR_DEFAULT,
        left_only: false,
        right_only: false,
    };
    let mut rest: Vec<String> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--left-only" => opts.left_only = true,
            "--right-only" => opts.right_only = true,
            // Without color, the dual and simple renderings are the same bytes.
            "--no-dual-color" | "--no-color" => {}
            "--creation-factor" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow!("option `--creation-factor` requires a value"))?;
                opts.creation_factor = parse_factor(v)?;
            }
            _ if a.starts_with("--creation-factor=") => {
                opts.creation_factor = parse_factor(&a["--creation-factor=".len()..])?;
            }
            "--" => bail!("pathspec limiting is not supported"),
            _ if a.starts_with('-') => bail!(
                "unsupported flag {a:?} (ported: --creation-factor, --left-only, \
                 --right-only, --no-dual-color, --no-color)"
            ),
            _ => rest.push(a.to_string()),
        }
        i += 1;
    }

    if opts.left_only && opts.right_only {
        // Upstream's `error()`, whose -1 return becomes git's exit status 255.
        eprintln!("error: options '--left-only' and '--right-only' cannot be used together");
        return Ok(ExitCode::from(255));
    }

    let repo = gix::discover(".")?;
    let Some((range1, range2)) = classify(&repo, &rest)? else {
        eprintln!("fatal: need two commit ranges\n");
        return Ok(ExitCode::from(129));
    };

    if repo.try_find_reference("refs/notes/commits")?.is_some() {
        bail!(
            "this repository has a refs/notes/commits ref; `git range-diff` shows notes \
             by default and note rendering is not ported"
        );
    }

    let mailmap = repo.open_mailmap();
    let mut a = read_patches(&repo, &range1, &mailmap)?;
    let mut b = read_patches(&repo, &range2, &mailmap)?;

    find_exact_matches(&mut a, &mut b);
    get_correspondences(&mut a, &mut b, opts.creation_factor);

    let mut rendered: Vec<u8> = Vec::new();
    output(&mut rendered, &mut a, &b, &opts)?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    out.write_all(&rendered)?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// `OPT_INTEGER` for `--creation-factor`.
fn parse_factor(s: &str) -> Result<i64> {
    s.parse::<i64>()
        .map_err(|_| anyhow!("invalid value {s:?} for `--creation-factor`"))
}

// ---------------------------------------------------------------------------
// Argument dispatch (builtin/range-diff.c)
// ---------------------------------------------------------------------------

/// Upstream's argument classification, in its exact precedence order: three
/// committishes, then two commit ranges, then one symmetric range. `Ok(None)`
/// means "need two commit ranges", the usage error.
fn classify(repo: &gix::Repository, args: &[String]) -> Result<Option<(String, String)>> {
    if args.len() > 2
        && committish(repo, &args[0])
        && committish(repo, &args[1])
        && committish(repo, &args[2])
    {
        if args.len() > 3 {
            bail!("pathspec limiting is not supported");
        }
        return Ok(Some((
            format!("{}..{}", args[0], args[1]),
            format!("{}..{}", args[0], args[2]),
        )));
    }
    if args.len() > 1 && is_range(repo, &args[0]) && is_range(repo, &args[1]) {
        if args.len() > 2 {
            bail!("pathspec limiting is not supported");
        }
        return Ok(Some((args[0].clone(), args[1].clone())));
    }
    if args.len() == 1 {
        if let Some(dots) = args[0].find("...") {
            let a = if dots == 0 { "HEAD" } else { &args[0][..dots] };
            let b = if args[0].len() > dots + 3 {
                &args[0][dots + 3..]
            } else {
                "HEAD"
            };
            return Ok(Some((format!("{b}..{a}"), format!("{a}..{b}"))));
        }
    }
    Ok(None)
}

/// `get_oid_committish()`: does `spec` name something that peels to a commit?
fn committish(repo: &gix::Repository, spec: &str) -> bool {
    resolve_commit(repo, spec).is_ok()
}

/// `is_range_diff_range()`: does `spec` name a range with both a positive and a
/// negative endpoint? Only the `<a>..<b>` and `<a>...<b>` spellings are
/// recognised here; `<rev>^!` and `<rev>^-<n>` are not, and fall through to the
/// usage error rather than being mis-parsed.
fn is_range(repo: &gix::Repository, spec: &str) -> bool {
    endpoints(repo, spec).is_ok()
}

fn resolve_commit(repo: &gix::Repository, spec: &str) -> Result<ObjectId> {
    let commit = repo
        .rev_parse_single(spec)?
        .object()?
        .peel_to_commit()
        .map_err(|e| anyhow!("{spec}: not a commit: {e}"))?;
    Ok(commit.id)
}

/// Split a range into the tips it includes and the commits it hides.
///
/// `<a>..<b>` hides `a` and includes `b`; `<a>...<b>` includes both and hides
/// their merge bases, matching how `git log` resolves the same spelling.
fn endpoints(repo: &gix::Repository, spec: &str) -> Result<(Vec<ObjectId>, Vec<ObjectId>)> {
    let or_head = |s: &str| if s.is_empty() { "HEAD" } else { s }.to_string();
    if let Some(dots) = spec.find("...") {
        let left = resolve_commit(repo, &or_head(&spec[..dots]))?;
        let right = resolve_commit(repo, &or_head(&spec[dots + 3..]))?;
        let bases: Vec<ObjectId> = repo
            .merge_bases_many(left, &[right])?
            .into_iter()
            .map(|id| id.detach())
            .collect();
        return Ok((vec![left, right], bases));
    }
    if let Some(dots) = spec.find("..") {
        let left = resolve_commit(repo, &or_head(&spec[..dots]))?;
        let right = resolve_commit(repo, &or_head(&spec[dots + 2..]))?;
        return Ok((vec![right], vec![left]));
    }
    bail!("{spec:?} is not a commit range of the form <a>..<b> or <a>...<b>")
}

// ---------------------------------------------------------------------------
// read_patches()
// ---------------------------------------------------------------------------

/// Render every non-merge commit of `range` into its canonical patch text.
fn read_patches(
    repo: &gix::Repository,
    range: &str,
    mailmap: &gix::mailmap::Snapshot,
) -> Result<Vec<Patch>> {
    let (tips, hidden) = endpoints(repo, range)?;
    let ids = ordered_commits(repo, tips, hidden)?;
    let mut out = Vec::with_capacity(ids.len());
    for (index, id) in ids.into_iter().enumerate() {
        out.push(build_patch(repo, id, index, mailmap)?);
    }
    Ok(out)
}

/// `--no-merges --reverse --date-order`: the commits of the range, oldest first,
/// merges dropped.
///
/// `--date-order` is topological order with a newest-commit-date-first
/// tie-break; this is Kahn's algorithm over the in-range child counts, which is
/// what `sort_in_topological_order()` runs.
fn ordered_commits(
    repo: &gix::Repository,
    tips: Vec<ObjectId>,
    hidden: Vec<ObjectId>,
) -> Result<Vec<ObjectId>> {
    let mut walk = repo.rev_walk(tips);
    if !hidden.is_empty() {
        walk = walk.with_hidden(hidden);
    }

    // The membership of the range, with parents and commit times.
    let mut order: Vec<ObjectId> = Vec::new();
    let mut parents: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    let mut times: HashMap<ObjectId, i64> = HashMap::new();
    for info in walk.all()? {
        let id = info?.id;
        let commit = repo.find_object(id)?.try_into_commit()?;
        times.insert(id, commit.time()?.seconds);
        parents.insert(id, commit.parent_ids().map(|p| p.detach()).collect());
        order.push(id);
    }

    // Child counts restricted to the range; upstream's `indegree` is 1-based.
    let mut indegree: HashMap<ObjectId, usize> = order.iter().map(|id| (*id, 1usize)).collect();
    for ps in parents.values() {
        for p in ps {
            if let Some(d) = indegree.get_mut(p) {
                *d += 1;
            }
        }
    }
    let seq: HashMap<ObjectId, usize> = order.iter().enumerate().map(|(n, id)| (*id, n)).collect();

    // Ready set: no children left inside the range. Newest commit date wins,
    // ties fall back to the (deterministic) traversal position.
    let mut ready: BinaryHeap<(i64, std::cmp::Reverse<usize>, ObjectId)> = order
        .iter()
        .filter(|id| indegree[*id] == 1)
        .map(|id| (times[id], std::cmp::Reverse(seq[id]), *id))
        .collect();

    let mut newest_first: Vec<ObjectId> = Vec::with_capacity(order.len());
    while let Some((_, _, id)) = ready.pop() {
        newest_first.push(id);
        for p in parents.get(&id).into_iter().flatten() {
            if let Some(d) = indegree.get_mut(p) {
                *d -= 1;
                if *d == 1 {
                    ready.push((times[p], std::cmp::Reverse(seq[p]), *p));
                }
            }
        }
    }

    newest_first.reverse();
    Ok(newest_first
        .into_iter()
        .filter(|id| parents[id].len() < 2)
        .collect())
}

/// Build the canonical patch text of one commit.
fn build_patch(
    repo: &gix::Repository,
    id: ObjectId,
    index: usize,
    mailmap: &gix::mailmap::Snapshot,
) -> Result<Patch> {
    let commit = repo.find_object(id)?.try_into_commit()?;

    // ` ## Metadata ##` — only the `Author:` line of `--pretty=medium` survives
    // upstream's header filter; `Date:` and `commit` are dropped.
    let mut text: Vec<u8> = Vec::new();
    let sig = commit.author()?;
    let raw_name: &[u8] = sig.name.as_ref();
    let raw_email: &[u8] = sig.email.as_ref();
    let resolved = mailmap.try_resolve(sig);
    let (name, email): (&[u8], &[u8]) = match &resolved {
        Some(s) => (s.name.as_ref(), s.email.as_ref()),
        None => (raw_name, raw_email),
    };
    text.extend_from_slice(b" ## Metadata ##\nAuthor: ");
    text.extend_from_slice(name);
    text.extend_from_slice(b" <");
    text.extend_from_slice(email);
    text.extend_from_slice(b">\n\n ## Commit message ##\n");

    let raw = commit.message_raw()?;
    for line in message_lines(raw) {
        // `pp_remainder()` writes a 4-space indent which `read_patches()` keeps,
        // then right-trims — so a blank message line collapses to nothing.
        if !line.is_empty() {
            text.extend_from_slice(b"    ");
            text.extend_from_slice(&line);
        }
        text.push(b'\n');
    }

    // One ` ## <path> ##` section per changed file, in path order — the order
    // `diff_tree()` walks both trees in.
    let new_tree = commit.tree()?;
    let old_tree = match commit.parent_ids().next() {
        Some(pid) => Some(pid.object()?.try_into_commit()?.tree()?),
        None => None,
    };
    let mut changes = repo.diff_tree_to_tree(
        old_tree.as_ref(),
        Some(&new_tree),
        gix::diff::Options::default(),
    )?;
    changes.sort_by(|x, y| change_path(x).cmp(change_path(y)));
    reject_renames(repo, old_tree.as_ref(), &new_tree, &changes, id)?;

    let mut diff_offset = 0usize;
    let mut diffsize = 0i64;
    for change in &changes {
        text.push(b'\n');
        if diff_offset == 0 {
            diff_offset = text.len();
        }
        emit_section(repo, &mut text, change, &mut diffsize)?;
    }

    Ok(Patch {
        index,
        abbrev: id.attach(repo).shorten()?.to_string(),
        subject: subject_of(raw),
        text,
        diff_offset,
        diffsize,
        matching: -1,
        shown: false,
    })
}

/// `diff.renames` is on for `git log`, so a detected rename changes both the
/// section header and the diff body. Find that case with gitoxide's tracker at
/// git's default 50% threshold and refuse, rather than silently emitting the
/// delete-plus-add rendering that rename detection would have replaced.
fn reject_renames(
    repo: &gix::Repository,
    old_tree: Option<&gix::Tree<'_>>,
    new_tree: &gix::Tree<'_>,
    changes: &[ChangeDetached],
    id: ObjectId,
) -> Result<()> {
    let has_add = changes
        .iter()
        .any(|c| matches!(c, ChangeDetached::Addition { .. }));
    let has_del = changes
        .iter()
        .any(|c| matches!(c, ChangeDetached::Deletion { .. }));
    if !(has_add && has_del) {
        return Ok(());
    }
    let tracked = repo.diff_tree_to_tree(
        old_tree,
        Some(new_tree),
        gix::diff::Options::default().with_rewrites(Some(gix::diff::Rewrites::default())),
    )?;
    if tracked
        .iter()
        .any(|c| matches!(c, ChangeDetached::Rewrite { .. }))
    {
        bail!("commit {id} contains a rename; git's diffcore-rename scoring is not ported");
    }
    Ok(())
}

/// Emit one ` ## <path> ##` section plus its rewritten hunks, tallying the
/// `diffsize` upstream accumulates one line at a time.
fn emit_section(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    change: &ChangeDetached,
    diffsize: &mut i64,
) -> Result<()> {
    let mut body: Vec<u8> = Vec::new();

    out.extend_from_slice(b" ## ");
    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            out.extend_from_slice(path);
            out.extend_from_slice(b" (new)");
            let content = content_of(repo, *id, entry_mode.is_commit())?;
            emit_hunks(&mut body, path, &[], &content, true, false)?;
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            out.extend_from_slice(path);
            out.extend_from_slice(b" (deleted)");
            let content = content_of(repo, *id, entry_mode.is_commit())?;
            emit_hunks(&mut body, path, &content, &[], false, true)?;
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            let path: &[u8] = location;
            out.extend_from_slice(path);
            let old_mode = previous_entry_mode.value();
            let new_mode = entry_mode.value();
            if old_mode != new_mode {
                out.extend_from_slice(
                    format!(" (mode change {old_mode:06o} => {new_mode:06o})").as_bytes(),
                );
            }
            // A pure mode change (identical content) has no hunks, like git.
            if previous_id != id {
                let old = content_of(repo, *previous_id, previous_entry_mode.is_commit())?;
                let new = content_of(repo, *id, entry_mode.is_commit())?;
                emit_hunks(&mut body, path, &old, &new, false, false)?;
            }
        }
        // Never produced: rewrite tracking is off, and `reject_renames()` has
        // already refused the commits where git would have found a rename.
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    }
    out.extend_from_slice(b" ##\n");

    *diffsize += 1 + body.iter().filter(|&&b| b == b'\n').count() as i64;
    out.extend_from_slice(&body);
    Ok(())
}

/// Render the hunks of one file with each header reduced to
/// `@@ <path>: <function>` (or a bare `@@` when there is no function context),
/// and each body line re-signed the way `read_patches()` re-signs the
/// `--output-indicator-*` markers it asked `git log` for.
///
/// `old_missing`/`new_missing` say which side is `/dev/null`; they matter only
/// for the `Binary files ... differ` labels.
fn emit_hunks(
    out: &mut Vec<u8>,
    path: &[u8],
    old: &[u8],
    new: &[u8],
    old_missing: bool,
    new_missing: bool,
) -> Result<()> {
    if is_binary(old) || is_binary(new) {
        let label = |missing: bool| {
            if missing {
                "/dev/null".to_string()
            } else {
                quote_c_style(path)
            }
        };
        out.extend_from_slice(
            format!(
                " Binary files {} and {} differ\n",
                label(old_missing),
                label(new_missing)
            )
            .as_bytes(),
        );
        return Ok(());
    }

    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let before: Vec<&[u8]> = input.before.iter().map(|&t| input.interner[t]).collect();
    let writer = InnerHunks {
        out,
        before,
        path: path.to_vec(),
    };
    UnifiedDiff::new(&diff, &input, writer, ContextSize::symmetrical(3)).consume()?;
    Ok(())
}

/// Writes the inner (per-commit) hunks in the canonical patch shape.
struct InnerHunks<'a> {
    out: &'a mut Vec<u8>,
    /// Pre-image lines, for resolving the hunk header's function context.
    before: Vec<&'a [u8]>,
    path: Vec<u8>,
}

impl InnerHunks<'_> {
    /// git's `def_ff()`, the default hunk-header function finder used when no
    /// `diff` attribute selects a userdiff driver: the nearest line above the
    /// hunk whose first byte is a letter, `_` or `$`, capped at 80 bytes and
    /// then right-trimmed.
    fn func(&self, hunk_start_0based: i64) -> Option<Vec<u8>> {
        let mut idx = hunk_start_0based - 1;
        while idx >= 0 {
            let line = self.before[idx as usize];
            match line.first() {
                Some(&first) if first.is_ascii_alphabetic() || first == b'_' || first == b'$' => {
                    let mut n = line.len().min(FUNC_BUF_SIZE);
                    while n > 0 && line[n - 1].is_ascii_whitespace() {
                        n -= 1;
                    }
                    return (n > 0).then(|| line[..n].to_vec());
                }
                _ => idx -= 1,
            }
        }
        None
    }
}

impl ConsumeHunk for InnerHunks<'_> {
    type Out = ();

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        // Upstream keeps only what follows the closing `@@` of the git hunk
        // header, prefixed with the file name — never the line numbers.
        self.out.extend_from_slice(b"@@");
        if let Some(func) = self.func(header.before_hunk_start as i64 - 1) {
            self.out.push(b' ');
            self.out.extend_from_slice(&self.path);
            self.out.extend_from_slice(b": ");
            self.out.extend_from_slice(&func);
        }
        self.out.push(b'\n');

        for &(kind, content) in lines {
            self.out.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            });
            self.out
                .extend_from_slice(content.strip_suffix(b"\n").unwrap_or(content));
            self.out.push(b'\n');
            if !content.ends_with(b"\n") {
                // git emits the missing newline itself, then the marker line,
                // which `read_patches()` sees as ordinary content.
                self.out
                    .extend_from_slice(b" \\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) {}
}

/// The bytes to diff: a blob from the object database, or a submodule rendered
/// the way `--submodule=short` renders it.
fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// git's `buffer_is_binary()`: a NUL byte within the first 8000 bytes.
fn is_binary(content: &[u8]) -> bool {
    content.iter().take(FIRST_FEW_BYTES).any(|&b| b == 0)
}

/// `quote_c_style()` under git's default `core.quotePath=true`.
fn quote_c_style(path: &[u8]) -> String {
    let needs = path
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return String::from_utf8_lossy(path).into_owned();
    }
    let mut s = String::from("\"");
    for &b in path {
        match b {
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            0x07 => s.push_str("\\a"),
            0x08 => s.push_str("\\b"),
            0x0c => s.push_str("\\f"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            0x0b => s.push_str("\\v"),
            _ if b < 0x20 || b >= 0x7f => s.push_str(&format!("\\{b:03o}")),
            _ => s.push(b as char),
        }
    }
    s.push('"');
    s
}

fn change_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

// ---------------------------------------------------------------------------
// Commit-message plumbing (pretty.c)
// ---------------------------------------------------------------------------

/// The message lines `pp_remainder()` prints at indent 4, each already
/// right-trimmed by `is_blank_line()`, with leading blank lines skipped by
/// `skip_blank_lines()` and trailing ones removed by the final `strbuf_rtrim()`.
fn message_lines(msg: &BStr) -> Vec<Vec<u8>> {
    let bytes: &[u8] = msg;
    let mut lines: Vec<Vec<u8>> = bytes
        .split(|&b| b == b'\n')
        .map(|l| trim_end_ws(l).to_vec())
        .collect();
    // Splitting a newline-terminated message yields a trailing empty element.
    if bytes.last() == Some(&b'\n') {
        lines.pop();
    }
    let first_content = lines
        .iter()
        .position(|l| !l.is_empty())
        .unwrap_or(lines.len());
    lines.drain(..first_content);
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// `pp_commit_easy(CMIT_FMT_ONELINE, ...)`: `format_subject()` with a single
/// space separator, i.e. the first paragraph folded onto one line.
fn subject_of(msg: &BStr) -> Vec<u8> {
    let mut title: Vec<u8> = Vec::new();
    for line in message_lines(msg) {
        if line.is_empty() {
            break;
        }
        if !title.is_empty() {
            title.push(b' ');
        }
        title.extend_from_slice(&line);
    }
    title
}

/// Strip trailing whitespace of git's `isspace` set.
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

// ---------------------------------------------------------------------------
// find_exact_matches() / get_correspondences() / linear-assignment.c
// ---------------------------------------------------------------------------

/// Pair off byte-identical diffs. Upstream's hashmap chains are LIFO, so when
/// the left range holds duplicates the highest index is matched first.
fn find_exact_matches(a: &mut [Patch], b: &mut [Patch]) {
    let mut map: HashMap<&[u8], Vec<usize>> = HashMap::new();
    for (i, p) in a.iter().enumerate() {
        map.entry(p.diff()).or_default().push(i);
    }
    // Collected first so the shared borrow of `a` ends before it is mutated.
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (j, p) in b.iter().enumerate() {
        if let Some(i) = map.get_mut(p.diff()).and_then(Vec::pop) {
            pairs.push((i, j));
        }
    }
    drop(map);
    for (i, j) in pairs {
        a[i].matching = j as i64;
        b[j].matching = i as i64;
    }
}

/// Upstream's `diffsize()`: hunk count plus line count of the diff-of-diffs at
/// three context lines, with plain xdiff settings — note that `xpparam_t pp` is
/// zeroed there, so unlike every other diff in git the indent heuristic is off.
fn diffsize(a: &[u8], b: &[u8]) -> i64 {
    let input = InternedInput::new(a, b);
    let mut diff = Diff::compute(Algorithm::Myers, &input);
    diff.postprocess_no_heuristic(&input);
    let counter = LineCounter { count: 0 };
    UnifiedDiff::new(&diff, &input, counter, ContextSize::symmetrical(3))
        .consume()
        .unwrap_or(COST_MAX)
}

/// Counts one per hunk header plus one per emitted line.
struct LineCounter {
    count: i64,
}

impl ConsumeHunk for LineCounter {
    type Out = i64;

    fn consume_hunk(
        &mut self,
        _header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        self.count += 1 + lines.len() as i64;
        Ok(())
    }

    fn finish(self) -> i64 {
        self.count
    }
}

/// Build and solve the cost matrix, recording the resulting correspondences.
fn get_correspondences(a: &mut [Patch], b: &mut [Patch], creation_factor: i64) {
    let n = a.len() + b.len();
    if n == 0 {
        return;
    }
    let mut cost = vec![0i64; n * n];

    for i in 0..a.len() {
        for j in 0..b.len() {
            cost[i + n * j] = if a[i].matching == j as i64 {
                0
            } else if a[i].matching < 0 && b[j].matching < 0 {
                diffsize(a[i].diff(), b[j].diff())
            } else {
                COST_MAX
            };
        }
        let c = if a[i].matching < 0 {
            a[i].diffsize * creation_factor / 100
        } else {
            COST_MAX
        };
        for j in b.len()..n {
            cost[i + n * j] = c;
        }
    }

    for j in 0..b.len() {
        let c = if b[j].matching < 0 {
            b[j].diffsize * creation_factor / 100
        } else {
            COST_MAX
        };
        for i in a.len()..n {
            cost[i + n * j] = c;
        }
    }

    for i in a.len()..n {
        for j in b.len()..n {
            cost[i + n * j] = 0;
        }
    }

    let mut a2b = vec![-1i64; n];
    let mut b2a = vec![-1i64; n];
    compute_assignment(n, n, &cost, &mut a2b, &mut b2a);

    for i in 0..a.len() {
        let j = a2b[i];
        if j >= 0 && (j as usize) < b.len() {
            a[i].matching = j;
            b[j as usize].matching = i as i64;
        }
    }
}

/// A port of `linear-assignment.c` — Jonker & Volgenant's shortest augmenting
/// path algorithm for the dense linear assignment problem.
///
/// `cost[column + column_count * row]` is the cost of assigning `column` to
/// `row`. `column2row` and `row2column` receive the assignment, `-1` where a
/// node stays unassigned. The control flow (including the two-phase augmenting
/// row reduction that re-queues in place, and the `goto update` that leaves `j`
/// holding the column the preceding scan left behind) is transcribed as-is.
fn compute_assignment(
    column_count: usize,
    row_count: usize,
    cost: &[i64],
    column2row: &mut [i64],
    row2column: &mut [i64],
) {
    let at = |column: usize, row: usize| cost[column + column_count * row];

    if column_count < 2 {
        column2row[..column_count].fill(0);
        row2column[..row_count].fill(0);
        return;
    }

    column2row[..column_count].fill(-1);
    row2column[..row_count].fill(-1);
    let mut v = vec![0i64; column_count];

    // Column reduction.
    for j in (0..column_count).rev() {
        let mut i1 = 0usize;
        for i in 1..row_count {
            if at(j, i1) > at(j, i) {
                i1 = i;
            }
        }
        v[j] = at(j, i1);
        if row2column[i1] == -1 {
            row2column[i1] = j as i64;
            column2row[j] = i1 as i64;
        } else {
            if row2column[i1] >= 0 {
                row2column[i1] = -2 - row2column[i1];
            }
            column2row[j] = -1;
        }
    }

    // Reduction transfer. `free_row` doubles as the work queue below, exactly as
    // upstream reuses the one allocation.
    let mut free_row = vec![0usize; row_count];
    let mut free_count = 0usize;
    for i in 0..row_count {
        let j1 = row2column[i];
        if j1 == -1 {
            free_row[free_count] = i;
            free_count += 1;
        } else if j1 < -1 {
            row2column[i] = -2 - j1;
        } else {
            let j1 = j1 as usize;
            // C's `!j1`: column 1 when j1 is 0, column 0 otherwise.
            let other = usize::from(j1 == 0);
            let mut min = at(other, i) - v[other];
            for j in 1..column_count {
                if j != j1 && min > at(j, i) - v[j] {
                    min = at(j, i) - v[j];
                }
            }
            v[j1] -= min;
        }
    }

    let expected_free = if column_count < row_count {
        row_count - column_count
    } else {
        0
    };
    if free_count == expected_free {
        return;
    }

    // Augmenting row reduction, two phases.
    for _phase in 0..2 {
        let mut k = 0usize;
        let saved_free_count = free_count;
        free_count = 0;
        while k < saved_free_count {
            let i = free_row[k];
            k += 1;

            let mut j1 = 0usize;
            let mut u1 = at(j1, i) - v[j1];
            let mut j2: i64 = -1;
            let mut u2 = i64::MAX;
            for j in 1..column_count {
                let c = at(j, i) - v[j];
                if u2 > c {
                    if u1 < c {
                        u2 = c;
                        j2 = j as i64;
                    } else {
                        u2 = u1;
                        u1 = c;
                        j2 = j1 as i64;
                        j1 = j;
                    }
                }
            }
            if j2 < 0 {
                j2 = j1 as i64;
                u2 = u1;
            }

            let mut i0 = column2row[j1];
            if u1 < u2 {
                v[j1] -= u2 - u1;
            } else if i0 >= 0 {
                j1 = j2 as usize;
                i0 = column2row[j1];
            }

            if i0 >= 0 {
                if u1 < u2 {
                    k -= 1;
                    free_row[k] = i0 as usize;
                } else {
                    free_row[free_count] = i0 as usize;
                    free_count += 1;
                }
            }
            row2column[i] = j1 as i64;
            column2row[j1] = i as i64;
        }
    }

    // Augmentation.
    let saved_free_count = free_count;
    let mut d = vec![0i64; column_count];
    let mut pred = vec![0usize; column_count];
    let mut col: Vec<usize> = vec![0; column_count];
    for f in 0..saved_free_count {
        let i1 = free_row[f];
        let mut low = 0usize;
        let mut up = 0usize;
        let mut last = 0usize;
        let mut min = 0i64;
        let mut j: i64 = -1;

        for jj in 0..column_count {
            d[jj] = at(jj, i1) - v[jj];
            pred[jj] = i1;
            col[jj] = jj;
        }

        // `do { ... } while (low == up)` with two `goto update` exits.
        loop {
            last = low;
            min = d[col[up]];
            up += 1;
            for k in up..column_count {
                j = col[k] as i64;
                let c = d[j as usize];
                if c <= min {
                    if c < min {
                        up = low;
                        min = c;
                    }
                    col[k] = col[up];
                    col[up] = j as usize;
                    up += 1;
                }
            }
            // Upstream jumps to `update` here without touching `j`, so the
            // augmenting path starts from whatever column the scan above left.
            if (low..up).any(|k| column2row[col[k]] == -1) {
                break;
            }

            // Scan a row: `do { ... } while (low != up)`.
            let mut jumped = false;
            loop {
                let j1 = col[low];
                low += 1;
                let i = column2row[j1] as usize;
                let u1 = at(j1, i) - v[j1] - min;
                for k in up..column_count {
                    j = col[k] as i64;
                    let c = at(j as usize, i) - v[j as usize] - u1;
                    if c < d[j as usize] {
                        d[j as usize] = c;
                        pred[j as usize] = i;
                        if c == min {
                            if column2row[j as usize] == -1 {
                                jumped = true;
                                break;
                            }
                            col[k] = col[up];
                            col[up] = j as usize;
                            up += 1;
                        }
                    }
                }
                if jumped || low == up {
                    break;
                }
            }
            if jumped || low != up {
                break;
            }
        }

        // Updating of the column pieces.
        for k in 0..last {
            let j1 = col[k];
            v[j1] += d[j1] - min;
        }

        // Augmentation. Upstream `BUG()`s on a negative `j`; there is nothing
        // sensible to do here either, so leave the assignment untouched.
        if j < 0 {
            continue;
        }
        loop {
            let i = pred[j as usize];
            column2row[j as usize] = i as i64;
            std::mem::swap(&mut j, &mut row2column[i]);
            if i1 == i {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// output()
// ---------------------------------------------------------------------------

/// Walk both ranges in the order of the right-hand side, placing each left-hand
/// commit that has no counterpart once all of its predecessors have been shown.
fn output(out: &mut Vec<u8>, a: &mut [Patch], b: &[Patch], opts: &Opts) -> Result<()> {
    let patch_no_width = decimal_width(1 + a.len().max(b.len()) as u64);
    let mut dashes: Option<String> = None;
    let mut i = 0usize;
    let mut j = 0usize;

    while i < a.len() || j < b.len() {
        // Skip all the already-shown commits from the LHS.
        while i < a.len() && a[i].shown {
            i += 1;
        }

        // Show an unmatched LHS commit whose predecessors were shown.
        if i < a.len() && a[i].matching < 0 {
            if !opts.right_only {
                pair_header(out, patch_no_width, &mut dashes, Some(&a[i]), None)?;
            }
            i += 1;
            continue;
        }

        // Show unmatched RHS commits.
        while j < b.len() && b[j].matching < 0 {
            if !opts.left_only {
                pair_header(out, patch_no_width, &mut dashes, None, Some(&b[j]))?;
            }
            j += 1;
        }

        // Show a matching LHS/RHS pair.
        if j < b.len() {
            let ai = b[j].matching as usize;
            pair_header(out, patch_no_width, &mut dashes, Some(&a[ai]), Some(&b[j]))?;
            patch_diff(out, &a[ai].text, &b[j].text)?;
            a[ai].shown = true;
            j += 1;
        }
    }
    Ok(())
}

/// `output_pair_header()` with color disabled: every color string is empty, so
/// the line reduces to the two index/abbreviation columns, the status character
/// and the one-line subject.
fn pair_header(
    out: &mut Vec<u8>,
    width: usize,
    dashes: &mut Option<String>,
    a: Option<&Patch>,
    b: Option<&Patch>,
) -> Result<()> {
    let anchor = a.or(b).expect("at least one side is present");
    if dashes.is_none() {
        *dashes = Some("-".repeat(anchor.abbrev.len()));
    }
    let dashes: &str = dashes.as_deref().expect("set just above");

    let status = match (a, b) {
        (Some(_), None) => b'<',
        (None, Some(_)) => b'>',
        (Some(x), Some(y)) if x.text != y.text => b'!',
        _ => b'=',
    };

    let mut line: Vec<u8> = Vec::new();
    match a {
        Some(p) => line.extend_from_slice(
            format!("{:>width$}:  {} ", p.index + 1, p.abbrev, width = width).as_bytes(),
        ),
        None => {
            line.extend_from_slice(format!("{:>width$}:  {dashes} ", "-", width = width).as_bytes())
        }
    }
    line.push(status);
    match b {
        Some(p) => line.extend_from_slice(
            format!(" {:>width$}:  {}", p.index + 1, p.abbrev, width = width).as_bytes(),
        ),
        None => {
            line.extend_from_slice(format!(" {:>width$}:  {dashes}", "-", width = width).as_bytes())
        }
    }
    line.push(b' ');
    line.extend_from_slice(&anchor.subject);
    line.push(b'\n');
    out.extend_from_slice(&line);
    Ok(())
}

/// `decimal_width()` from pager.c.
fn decimal_width(mut number: u64) -> usize {
    let mut width = 1;
    while number >= 10 {
        number /= 10;
        width += 1;
    }
    width
}

/// The diff-of-diffs: four-space indented, no file headers, and a hunk header
/// of `@@` plus the section name the `section_headers` driver finds.
fn patch_diff(out: &mut Vec<u8>, a: &[u8], b: &[u8]) -> Result<()> {
    let input = InternedInput::new(a, b);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    let before: Vec<&[u8]> = input.before.iter().map(|&t| input.interner[t]).collect();

    let writer = OuterHunks {
        out,
        before,
        func_line: Vec::new(),
        funclineprev: -1,
    };
    UnifiedDiff::new(&diff, &input, writer, ContextSize::symmetrical(3)).consume()?;
    Ok(())
}

/// Writes the outer hunks, carrying `func_line` and `funclineprev` across hunks
/// the way `xdl_emit_diff()` does.
struct OuterHunks<'a> {
    out: &'a mut Vec<u8>,
    before: Vec<&'a [u8]>,
    /// Deliberately *not* reset per hunk: `get_func_line()` only overwrites its
    /// buffer on a match, so a hunk with no match repeats the previous name.
    func_line: Vec<u8>,
    /// The `s1 - 1` of the previous hunk, the exclusive limit of the search.
    funclineprev: i64,
}

impl ConsumeHunk for OuterHunks<'_> {
    type Out = ();

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        let s1 = header.before_hunk_start as i64 - 1;
        if let Some(f) = get_func_line(&self.before, s1 - 1, self.funclineprev) {
            self.func_line = f;
        }
        self.funclineprev = s1 - 1;

        self.out.extend_from_slice(INDENT);
        self.out.extend_from_slice(b"@@");
        if !self.func_line.is_empty() {
            self.out.push(b' ');
            self.out.extend_from_slice(&self.func_line);
        }
        self.out.push(b'\n');

        // `emit_line_0()` writes the prefix, the sign, then the record verbatim
        // — the patch text always ends its lines, so nothing is appended.
        for &(kind, content) in lines {
            self.out.extend_from_slice(INDENT);
            self.out.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => b'+',
                DiffLineKind::Remove => b'-',
            });
            self.out.extend_from_slice(content);
            if !content.ends_with(b"\n") {
                self.out.push(b'\n');
            }
        }
        Ok(())
    }

    fn finish(self) {}
}

/// `get_func_line()`: scan `records` from `start` towards `limit` (exclusive)
/// for the first line the section-header driver matches.
fn get_func_line(records: &[&[u8]], start: i64, limit: i64) -> Option<Vec<u8>> {
    let step: i64 = if start > limit { -1 } else { 1 };
    let mut l = start;
    while l != limit && 0 <= l && (l as usize) < records.len() {
        if let Some(f) = section_name(records[l as usize]) {
            return Some(f);
        }
        l += step;
    }
    None
}

/// Upstream's `section_headers` userdiff driver run through `ff_regexp()`: try
/// `^ ## (.*) ##$` then `^.?@@ (.*)$` against the record with its line
/// terminator excluded, take capture group 1, cap it at 80 bytes, then trim
/// trailing whitespace.
fn section_name(record: &[u8]) -> Option<Vec<u8>> {
    let mut len = record.len();
    if len > 0 && record[len - 1] == b'\n' {
        if len > 1 && record[len - 2] == b'\r' {
            len -= 2;
        } else {
            len -= 1;
        }
    }
    let line = &record[..len];

    let group = match_section(line).or_else(|| match_hunk(line))?;
    let mut n = group.len().min(FUNC_BUF_SIZE);
    while n > 0 && group[n - 1].is_ascii_whitespace() {
        n -= 1;
    }
    Some(group[..n].to_vec())
}

/// `^ ## (.*) ##$`. `.*` is greedy and `$` anchors, so the group runs from just
/// after the opening ` ## ` to just before the final ` ##`.
fn match_section(line: &[u8]) -> Option<&[u8]> {
    (line.len() >= 7 && line.starts_with(b" ## ") && line.ends_with(b" ##"))
        .then(|| &line[4..line.len() - 3])
}

/// `^.?@@ (.*)$`. The optional leading character is greedy, so a one-character
/// diff marker is consumed in preference to matching `@@ ` at offset zero.
fn match_hunk(line: &[u8]) -> Option<&[u8]> {
    if line.len() >= 4 && line[1..].starts_with(b"@@ ") {
        return Some(&line[4..]);
    }
    if line.starts_with(b"@@ ") {
        return Some(&line[3..]);
    }
    None
}
