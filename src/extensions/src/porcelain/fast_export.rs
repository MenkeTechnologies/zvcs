//! `git fast-export` — dump revisions in the `git fast-import` stream format.
//!
//! Backed entirely by the vendored gitoxide (`src/ported`). The commit order is
//! stock git's `rev-list --topo-order --reverse`, produced by
//! `gix_traverse::commit::topo` (a port of git's `sort_in_topological_order`);
//! the per-commit ref label is git's `--source` decoration, propagated over a
//! commit-date-ordered walk exactly as `add_parents_to_list` does it. The
//! tree-vs-tree walk is implemented here rather than through
//! `gix::Repository::diff_tree_to_tree` so the change order matches git's
//! recursive `diff_tree_oid` emission order, which the stream's `M`/`D` line
//! order and the blob export order both depend on.
//!
//! ### Covered (byte-identical stdout, exit code and marks file against stock git)
//!
//! * `fast-export --all`, `fast-export <rev>...`, `<a>..<b>`, `^<rev>`
//! * blob / commit / `reset` / lightweight-tag / annotated-tag stanzas, including
//!   the trailing `reset`+`tag` block emitted in git's reverse-cmdline order
//! * `--no-data`, `--full-tree`, `--use-done-feature`, `--show-original-ids`,
//!   `--mark-tags`, `--progress=<n>`, `--export-marks=<file>`
//! * `--signed-tags=(verbatim|warn|warn-verbatim|warn-strip|strip|abort)`
//! * `--tag-of-filtered-object=(abort|drop)`
//! * `--signed-commits=(strip|warn-strip)`, `--reencode=(no|abort)`
//! * `--fake-missing-tagger`
//!
//! ### Honest limitations (bailed on with a precise message, never silently ignored)
//!
//! * `-M`/`-C` — rename/copy detection needs `diffcore-rename`, which the
//!   vendored `gix-diff` does not expose in a form that reproduces git's
//!   `R`/`C` stanzas.
//! * `--anonymize`, `--anonymize-map=<...>` — git's anonymization uses a seeded
//!   token generator whose output is not reproducible from here.
//! * `--reencode=yes` — needs iconv; no re-encoding substrate is vendored.
//! * `--signed-commits=(verbatim|warn-verbatim|abort)` — emitting `gpgsig`
//!   stanzas requires the experimental signed-commit stream extension.
//! * `--tag-of-filtered-object=rewrite` — needs rev-list parent rewriting.
//! * `--import-marks=<file>`, `--import-marks-if-exists=<file>`,
//!   `--reference-excluded-parents`, `--refspec=<refspec>`.
//! * pathspec filtering (`-- <path>...`) and symmetric-difference ranges.

use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::tree::{EntryKind, EntryMode};

/// How a signature found in a tag (or commit) is dealt with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SignedMode {
    Verbatim,
    WarnVerbatim,
    WarnStrip,
    Strip,
    Abort,
}

/// `--tag-of-filtered-object`: what to do with a tag whose object was not exported.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FilteredTagMode {
    Abort,
    Drop,
}

/// Parsed command-line options for a single `fast-export` invocation.
struct Opts {
    no_data: bool,           // --no-data: refer to blobs by hash, emit no blob stanzas
    full_tree: bool,         // --full-tree: `deleteall` plus the whole tree per commit
    use_done: bool,          // --use-done-feature: `feature done` header and `done` trailer
    show_original_ids: bool, // --show-original-ids: `original-oid <sha>` directives
    mark_tags: bool,         // --mark-tags: give annotated tags a mark too
    fake_missing_tagger: bool, // --fake-missing-tagger
    progress: Option<u64>,   // --progress=<n>: a `progress` line every <n> objects
    export_marks: Option<String>, // --export-marks=<file>
    signed_tags: SignedMode, // --signed-tags=<mode>
    filtered_tag: FilteredTagMode, // --tag-of-filtered-object=<mode>
}

/// The tagger git invents for a tag object that has none, when asked to.
const FAKE_TAGGER: &str = "tagger <unknown> <unknown> 0 +0000";

/// `git fast-export` — see the module documentation for the covered surface.
pub fn fast_export(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first().map(String::as_str) {
        Some("fast-export") => &args[1..],
        _ => args,
    };

    let mut opts = Opts {
        no_data: false,
        full_tree: false,
        use_done: false,
        show_original_ids: false,
        mark_tags: false,
        fake_missing_tagger: false,
        progress: None,
        export_marks: None,
        signed_tags: SignedMode::Abort,
        filtered_tag: FilteredTagMode::Abort,
    };

    let mut use_all = false;
    let mut revs: Vec<String> = Vec::new();
    let mut hidden_specs: Vec<String> = Vec::new();

    for a in args {
        let s = a.as_str();
        match s {
            "--all" => use_all = true,
            "--no-data" => opts.no_data = true,
            "--full-tree" => opts.full_tree = true,
            "--use-done-feature" => opts.use_done = true,
            "--show-original-ids" => opts.show_original_ids = true,
            "--mark-tags" => opts.mark_tags = true,
            "--fake-missing-tagger" => opts.fake_missing_tagger = true,
            "--" => bail!("pathspec filtering is not supported"),
            _ if s.starts_with("--progress=") => {
                let v = &s["--progress=".len()..];
                opts.progress = Some(
                    v.parse::<u64>()
                        .map_err(|_| anyhow!("invalid --progress value {v:?}"))?,
                );
            }
            _ if s.starts_with("--export-marks=") => {
                opts.export_marks = Some(s["--export-marks=".len()..].to_string());
            }
            _ if s.starts_with("--signed-tags=") => {
                opts.signed_tags = match &s["--signed-tags=".len()..] {
                    "verbatim" => SignedMode::Verbatim,
                    "warn" | "warn-verbatim" => SignedMode::WarnVerbatim,
                    "warn-strip" => SignedMode::WarnStrip,
                    "strip" => SignedMode::Strip,
                    "abort" => SignedMode::Abort,
                    other => bail!("unknown --signed-tags mode {other:?}"),
                };
            }
            _ if s.starts_with("--signed-commits=") => match &s["--signed-commits=".len()..] {
                // `strip` is git's default and is what this port does by construction:
                // commit headers other than author/committer are never emitted.
                "strip" | "warn-strip" => {}
                other => bail!(
                    "unsupported --signed-commits mode {other:?} \
                     (ported: strip, warn-strip)"
                ),
            },
            _ if s.starts_with("--reencode=") => match &s["--reencode=".len()..] {
                "no" | "abort" => {}
                other => bail!("unsupported --reencode mode {other:?} (ported: no, abort)"),
            },
            _ if s.starts_with("--tag-of-filtered-object=") => {
                opts.filtered_tag = match &s["--tag-of-filtered-object=".len()..] {
                    "abort" => FilteredTagMode::Abort,
                    "drop" => FilteredTagMode::Drop,
                    other => bail!(
                        "unsupported --tag-of-filtered-object mode {other:?} (ported: abort, drop)"
                    ),
                };
            }
            _ if s.starts_with('^') => hidden_specs.push(s[1..].to_string()),
            _ if s.starts_with('-') => bail!(
                "unsupported flag {s:?} (ported: --all, --no-data, --full-tree, \
                 --use-done-feature, --show-original-ids, --mark-tags, \
                 --fake-missing-tagger, --progress=<n>, --export-marks=<file>, \
                 --signed-tags=<mode>, --tag-of-filtered-object=<mode>, \
                 --signed-commits=<mode>, --reencode=<mode>)"
            ),
            _ if s.contains("...") => bail!("symmetric-difference range {s:?} is not supported"),
            _ if s.contains("..") => {
                let (left, right) = s.split_once("..").expect("checked by the guard above");
                hidden_specs.push(if left.is_empty() { "HEAD" } else { left }.to_string());
                revs.push(if right.is_empty() { "HEAD" } else { right }.to_string());
            }
            _ => revs.push(s.to_string()),
        }
    }

    let repo = gix::discover(".")?;

    // ---- The `rev_cmdline` list: every positive ref, in git's iteration order. ----
    let mut cmdline: Vec<(BString, ObjectId)> = Vec::new();
    if use_all {
        let mut names: Vec<BString> = Vec::new();
        for reference in repo.references()?.all()? {
            let reference = reference.map_err(|e| anyhow!("{e}"))?;
            names.push(reference.name().as_bstr().to_owned());
        }
        names.sort();
        for name in names {
            let spec = name.to_str().map_err(|_| anyhow!("non-UTF-8 ref {name:?}"))?;
            let id = repo.rev_parse_single(spec)?.detach();
            cmdline.push((name, id));
        }
    }
    for spec in &revs {
        // git's `get_tags_and_duplicates` only considers args that dwim to a ref;
        // a raw commit id would leave the commit without a name to print.
        let reference = repo
            .find_reference(spec.as_str())
            .map_err(|_| anyhow!("{spec:?} does not name a ref; exporting anonymous revisions is not supported"))?;
        let name = reference.name().as_bstr().to_owned();
        let id = repo.rev_parse_single(spec.as_str())?.detach();
        cmdline.push((name, id));
    }
    if cmdline.is_empty() {
        bail!("no revisions given (use --all or name a ref)");
    }

    let mut hidden: Vec<ObjectId> = Vec::new();
    for spec in &hidden_specs {
        hidden.push(commit_of_spec(&repo, spec)?);
    }

    // ---- Ref bookkeeping, mirroring `get_tags_and_duplicates`. ----
    // `sources` is git's `--source` decoration: the ref name a commit is printed
    // under. The first cmdline ref reaching a commit wins; later ones become
    // standalone `reset` stanzas. Annotated tags claim a source too, but never
    // produce a duplicate `reset`.
    let mut sources: HashMap<ObjectId, BString> = HashMap::new();
    let mut extra_refs: Vec<(BString, ObjectId)> = Vec::new();
    let mut tag_refs: Vec<(BString, ObjectId)> = Vec::new();
    let mut tips: Vec<ObjectId> = Vec::new();

    for (name, target) in &cmdline {
        let object = repo.find_object(*target)?;
        let is_tag = object.kind == gix::object::Kind::Tag;
        let Ok(commit) = object.peel_to_commit() else {
            continue; // a ref to a blob or tree is not exportable, as in git
        };
        let commit_id = commit.id;
        if is_tag {
            tag_refs.push((name.clone(), *target));
        }
        if sources.contains_key(&commit_id) {
            if !is_tag {
                extra_refs.push((name.clone(), commit_id));
            }
        } else {
            sources.insert(commit_id, name.clone());
        }
        tips.push(commit_id);
    }
    tips.sort();
    tips.dedup();

    // ---- Source propagation over the commit-date walk git uses for it. ----
    {
        let mut platform = repo
            .rev_walk(tips.clone())
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ));
        if !hidden.is_empty() {
            platform = platform.with_hidden(hidden.clone());
        }
        for info in platform.all()? {
            let info = info?;
            let Some(src) = sources.get(&info.id).cloned() else {
                continue;
            };
            for parent in &info.parent_ids {
                sources.entry(*parent).or_insert_with(|| src.clone());
            }
        }
    }

    // ---- Emission order: `rev-list --topo-order --reverse`. ----
    let topo = gix::traverse::commit::topo::Builder::from_iters(
        &repo.objects,
        tips.clone(),
        Some(hidden.clone()),
    )
    .sorting(gix::traverse::commit::topo::Sorting::TopoOrder)
    .build()?;
    let mut order: Vec<gix::traverse::commit::Info> = Vec::new();
    for info in topo {
        order.push(info?);
    }
    order.reverse();

    let mut st = State {
        out: Vec::new(),
        marks: HashMap::new(),
        commit_marks: Vec::new(),
        last_mark: 0,
        counter: 0,
    };

    if opts.use_done {
        st.out.extend_from_slice(b"feature done\n");
    }

    for info in &order {
        emit_commit(&repo, info, &opts, &sources, &mut st)?;
    }

    // ---- Trailing `reset`/`tag` block, in git's reverse-cmdline order. ----
    for (name, commit_id) in extra_refs.iter().rev() {
        let Some(mark) = st.marks.get(commit_id) else {
            continue; // the commit was excluded from this export
        };
        st.out.extend_from_slice(b"reset ");
        st.out.extend_from_slice(name);
        st.out
            .extend_from_slice(format!("\nfrom :{mark}\n\n").as_bytes());
    }
    for (name, tag_id) in tag_refs.iter().rev() {
        emit_tag(&repo, name.as_bstr(), *tag_id, &opts, &mut st)?;
    }

    if opts.use_done {
        st.out.extend_from_slice(b"done\n");
    }

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&st.out)?;
    stdout.flush()?;

    if let Some(path) = &opts.export_marks {
        if !st.commit_marks.is_empty() {
            let mut buf = String::new();
            for (mark, id) in &st.commit_marks {
                buf.push_str(&format!(":{mark} {id}\n"));
            }
            std::fs::write(path, buf)?;
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Mutable stream state shared by the blob/commit/tag emitters.
struct State {
    out: Vec<u8>,
    /// Mark assigned to every already-exported blob, commit and (with
    /// `--mark-tags`) tag object.
    marks: HashMap<ObjectId, u32>,
    /// Commit marks in assignment order — the only ones `--export-marks` dumps.
    commit_marks: Vec<(u32, ObjectId)>,
    last_mark: u32,
    /// git's `show_progress` counter: one tick per exported blob and commit.
    counter: u64,
}

impl State {
    /// git's `mark_next_object`.
    fn next_mark(&mut self, id: ObjectId) -> u32 {
        self.last_mark += 1;
        self.marks.insert(id, self.last_mark);
        self.last_mark
    }

    /// git's `show_progress`, called after each exported blob and commit.
    fn tick(&mut self, opts: &Opts) {
        self.counter += 1;
        if let Some(n) = opts.progress {
            if n != 0 && self.counter % n == 0 {
                self.out
                    .extend_from_slice(format!("progress {} objects\n", self.counter).as_bytes());
            }
        }
    }
}

/// Resolve a revision to the id of the commit it names, peeling tags.
fn commit_of_spec(repo: &gix::Repository, spec: &str) -> Result<ObjectId> {
    Ok(repo
        .rev_parse_single(spec)?
        .object()?
        .peel_to_commit()
        .map_err(|e| anyhow!("{spec}: not a commit: {e}"))?
        .id)
}

/// Emit one commit: its new blobs first, then the `commit` stanza.
fn emit_commit(
    repo: &gix::Repository,
    info: &gix::traverse::commit::Info,
    opts: &Opts,
    sources: &HashMap<ObjectId, BString>,
    st: &mut State,
) -> Result<()> {
    let id = info.id;
    let data = repo.find_object(id)?.data.clone();
    let (headers, message) = split_object(&data);
    let tree = header_value(headers, b"tree")
        .ok_or_else(|| anyhow!("commit {id} has no tree header"))?;
    let tree = ObjectId::from_hex(tree).map_err(|e| anyhow!("commit {id}: bad tree id: {e}"))?;
    let author = header_line(headers, b"author")
        .ok_or_else(|| anyhow!("commit {id} has no author header"))?;
    let committer = header_line(headers, b"committer")
        .ok_or_else(|| anyhow!("commit {id} has no committer header"))?;
    let parents: Vec<ObjectId> = info.parent_ids.iter().copied().collect();

    // `--full-tree` diffs against the empty tree so every path is (re-)stated.
    let base = if opts.full_tree || parents.is_empty() {
        None
    } else {
        Some(repo.find_object(parents[0])?.peel_to_tree()?.id)
    };
    let changes = collect(repo, base, Some(tree))?;

    // git exports every referenced blob before the commit that first names it,
    // walking the diff queue in order.
    if !opts.no_data {
        for c in &changes {
            if let Some(new) = c.new {
                if new.mode.kind() != EntryKind::Commit {
                    emit_blob(repo, new.id, opts, st)?;
                }
            }
        }
    }

    let refname = sources
        .get(&id)
        .ok_or_else(|| anyhow!("commit {id} is not reachable from any named ref"))?;

    let mark = st.next_mark(id);
    st.commit_marks.push((mark, id));

    if parents.is_empty() {
        st.out.extend_from_slice(b"reset ");
        st.out.extend_from_slice(refname);
        st.out.push(b'\n');
    }
    st.out.extend_from_slice(b"commit ");
    st.out.extend_from_slice(refname);
    st.out
        .extend_from_slice(format!("\nmark :{mark}\n").as_bytes());
    if opts.show_original_ids {
        st.out
            .extend_from_slice(format!("original-oid {id}\n").as_bytes());
    }
    st.out.extend_from_slice(author);
    st.out.push(b'\n');
    st.out.extend_from_slice(committer);
    st.out.push(b'\n');
    st.out
        .extend_from_slice(format!("data {}\n", message.len()).as_bytes());
    st.out.extend_from_slice(message);

    // Parents that were not exported are skipped entirely; the first *printed*
    // one is `from`, the rest are `merge`.
    let mut printed = 0usize;
    for p in &parents {
        let Some(pmark) = st.marks.get(p).copied() else {
            continue;
        };
        st.out
            .extend_from_slice(if printed == 0 { b"from " } else { b"merge " });
        st.out.extend_from_slice(format!(":{pmark}\n").as_bytes());
        printed += 1;
    }

    if opts.full_tree {
        st.out.extend_from_slice(b"deleteall\n");
    }
    for c in &changes {
        render_change(&mut st.out, c, opts, &st.marks)?;
    }
    st.out.push(b'\n');
    st.tick(opts);
    Ok(())
}

/// git's `export_blob`: a `blob` stanza, once per distinct object.
fn emit_blob(repo: &gix::Repository, id: ObjectId, opts: &Opts, st: &mut State) -> Result<()> {
    if st.marks.contains_key(&id) {
        return Ok(());
    }
    let data = repo.find_object(id)?.data.clone();
    let mark = st.next_mark(id);
    st.out
        .extend_from_slice(format!("blob\nmark :{mark}\n").as_bytes());
    if opts.show_original_ids {
        st.out
            .extend_from_slice(format!("original-oid {id}\n").as_bytes());
    }
    st.out
        .extend_from_slice(format!("data {}\n", data.len()).as_bytes());
    st.out.extend_from_slice(&data);
    st.out.push(b'\n');
    st.tick(opts);
    Ok(())
}

/// git's `handle_tag`: the `tag` stanza for an annotated tag.
fn emit_tag(
    repo: &gix::Repository,
    full_name: &BStr,
    tag_id: ObjectId,
    opts: &Opts,
    st: &mut State,
) -> Result<()> {
    let data = repo.find_object(tag_id)?.data.clone();
    let (headers, mut message) = split_object(&data);
    let target = header_value(headers, b"object")
        .ok_or_else(|| anyhow!("tag {tag_id} has no object header"))?;
    let target = ObjectId::from_hex(target).map_err(|e| anyhow!("tag {tag_id}: {e}"))?;
    if header_value(headers, b"type") == Some(&b"tag"[..]) {
        bail!("nested tags are not supported (tag {tag_id} tags another tag)");
    }
    let commit_id = repo.find_object(target)?.peel_to_commit()?.id;

    let Some(mark) = st.marks.get(&commit_id).copied() else {
        return match opts.filtered_tag {
            FilteredTagMode::Drop => Ok(()),
            FilteredTagMode::Abort => bail!(
                "tag {tag_id} tags unexported object; \
                 use --tag-of-filtered-object=<mode> to handle it"
            ),
        };
    };

    // git looks for the signature block and applies --signed-tags to it.
    if let Some(pos) = find_sub(message, b"\n-----BEGIN PGP SIGNATURE-----\n") {
        match opts.signed_tags {
            SignedMode::Abort => bail!(
                "encountered signed tag {tag_id}; \
                 use --signed-tags=<mode> to handle it"
            ),
            SignedMode::WarnVerbatim => eprintln!("warning: exporting signed tag {tag_id}"),
            SignedMode::Verbatim => {}
            SignedMode::WarnStrip => {
                eprintln!("warning: stripping signature from tag {tag_id}");
                message = &message[..pos + 1];
            }
            SignedMode::Strip => message = &message[..pos + 1],
        }
    }

    let full: &[u8] = full_name;
    let short = full.strip_prefix(&b"refs/tags/"[..]).unwrap_or(full);
    st.out.extend_from_slice(b"tag ");
    st.out.extend_from_slice(short);
    st.out.push(b'\n');
    if opts.mark_tags {
        let tmark = st.next_mark(tag_id);
        st.out
            .extend_from_slice(format!("mark :{tmark}\n").as_bytes());
    }
    st.out
        .extend_from_slice(format!("from :{mark}\n").as_bytes());
    if opts.show_original_ids {
        st.out
            .extend_from_slice(format!("original-oid {tag_id}\n").as_bytes());
    }
    match header_line(headers, b"tagger") {
        Some(line) => {
            st.out.extend_from_slice(line);
            st.out.push(b'\n');
        }
        None if opts.fake_missing_tagger => {
            st.out.extend_from_slice(FAKE_TAGGER.as_bytes());
            st.out.push(b'\n');
        }
        None => {}
    }
    st.out
        .extend_from_slice(format!("data {}\n", message.len()).as_bytes());
    st.out.extend_from_slice(message);
    st.out.push(b'\n');
    Ok(())
}

// ---------------------------------------------------------------------------
// Raw object parsing
// ---------------------------------------------------------------------------

/// Split a commit or tag object into its header block (each line still carrying
/// its terminating newline) and the message that follows the blank line.
fn split_object(data: &[u8]) -> (&[u8], &[u8]) {
    match find_sub(data, b"\n\n") {
        Some(i) => (&data[..i + 1], &data[i + 2..]),
        None => (data, &[]),
    }
}

/// The complete `"<name> <value>"` header line, without its newline.
///
/// Continuation lines (those starting with a space, as `gpgsig` uses) are skipped
/// so they can never be mistaken for a header of their own.
fn header_line<'a>(headers: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    for line in headers.split(|b| *b == b'\n') {
        if line.first() == Some(&b' ') {
            continue;
        }
        if line.len() > name.len() && line.starts_with(name) && line[name.len()] == b' ' {
            return Some(line);
        }
    }
    None
}

/// Just the value part of a header line.
fn header_value<'a>(headers: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    header_line(headers, name).map(|line| &line[name.len() + 1..])
}

/// Index of the first occurrence of `needle` in `haystack`.
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// Tree diff (recursive, git's emission order)
// ---------------------------------------------------------------------------

/// One side of a change: the entry as it exists in that tree.
#[derive(Clone, Copy)]
struct Side {
    mode: EntryMode,
    id: ObjectId,
}

struct Change {
    new: Option<Side>,
    path: BString,
}

/// A tree entry, materialised so the borrow on the tree buffer ends before we recurse.
struct Entry {
    mode: EntryMode,
    name: BString,
    id: ObjectId,
}

fn read_entries(repo: &gix::Repository, id: Option<ObjectId>) -> Result<Vec<Entry>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let tree = repo.find_tree(id)?;
    Ok(tree
        .decode()?
        .entries
        .iter()
        .map(|e| Entry {
            mode: e.mode,
            name: BString::from(e.filename.to_vec()),
            id: e.oid.to_owned(),
        })
        .collect())
}

/// git's `tree-entry-comparison`: names compare byte-wise with an implicit `/`
/// appended to tree entries.
fn entry_cmp(a: &Entry, b: &Entry) -> Ordering {
    let common = a.name.len().min(b.name.len());
    match a.name[..common].cmp(&b.name[..common]) {
        Ordering::Equal => {
            let ac = a.name.get(common).copied().or(a.mode.is_tree().then_some(b'/'));
            let bc = b.name.get(common).copied().or(b.mode.is_tree().then_some(b'/'));
            ac.cmp(&bc)
        }
        other => other,
    }
}

/// Every change turning `old` into `new`, recursively, in git's emission order.
///
/// Trees themselves are never reported: `fast-export` always sets
/// `diffopt.flags.recursive`, so only leaves reach the `M`/`D` renderer.
fn collect(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    walk(repo, old, new, BStr::new(""), &mut out)?;
    Ok(out)
}

fn walk(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    prefix: &BStr,
    out: &mut Vec<Change>,
) -> Result<()> {
    let lhs = read_entries(repo, old)?;
    let rhs = read_entries(repo, new)?;
    let (mut i, mut j) = (0usize, 0usize);

    while i < lhs.len() || j < rhs.len() {
        let order = match (lhs.get(i), rhs.get(j)) {
            (Some(a), Some(b)) => entry_cmp(a, b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => unreachable!("loop condition guarantees one side has an entry"),
        };
        match order {
            Ordering::Equal => {
                let (a, b) = (&lhs[i], &rhs[j]);
                i += 1;
                j += 1;
                if a.mode == b.mode && a.id == b.id {
                    continue;
                }
                let path = join(prefix, a.name.as_bstr());
                if a.mode.is_tree() {
                    walk(repo, Some(a.id), Some(b.id), path.as_bstr(), out)?;
                } else {
                    out.push(Change {
                        new: Some(side(b)),
                        path,
                    });
                }
            }
            Ordering::Less => {
                let a = &lhs[i];
                i += 1;
                let path = join(prefix, a.name.as_bstr());
                if a.mode.is_tree() {
                    walk(repo, Some(a.id), None, path.as_bstr(), out)?;
                } else {
                    out.push(Change { new: None, path });
                }
            }
            Ordering::Greater => {
                let b = &rhs[j];
                j += 1;
                let path = join(prefix, b.name.as_bstr());
                if b.mode.is_tree() {
                    walk(repo, None, Some(b.id), path.as_bstr(), out)?;
                } else {
                    out.push(Change {
                        new: Some(side(b)),
                        path,
                    });
                }
            }
        }
    }
    Ok(())
}

fn side(e: &Entry) -> Side {
    Side {
        mode: e.mode,
        id: e.id,
    }
}

fn join(prefix: &BStr, name: &BStr) -> BString {
    let mut p = BString::from(prefix.to_vec());
    if !p.is_empty() {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// git's `show_filemodify`: `D <path>` for a removal, `M <mode> <ref> <path>`
/// otherwise, where `<ref>` is a mark for exported blobs and a raw hash for
/// gitlinks and `--no-data`.
fn render_change(
    out: &mut Vec<u8>,
    c: &Change,
    opts: &Opts,
    marks: &HashMap<ObjectId, u32>,
) -> Result<()> {
    match c.new {
        None => {
            out.extend_from_slice(b"D ");
            print_path(out, c.path.as_bstr());
            out.push(b'\n');
        }
        Some(new) => {
            let mode = new.mode.value();
            let reference = if opts.no_data || new.mode.kind() == EntryKind::Commit {
                new.id.to_hex().to_string()
            } else {
                let mark = marks
                    .get(&new.id)
                    .ok_or_else(|| anyhow!("blob {} was not exported", new.id))?;
                format!(":{mark}")
            };
            out.extend_from_slice(format!("M {mode:06o} {reference} ").as_bytes());
            print_path(out, c.path.as_bstr());
            out.push(b'\n');
        }
    }
    Ok(())
}

/// git's `print_path`: C-style quoting when a byte needs escaping, plain double
/// quotes when the only special character is a space, bare otherwise.
fn print_path(out: &mut Vec<u8>, path: &BStr) {
    let needs_quote = path
        .iter()
        .any(|b| *b < 0x20 || *b >= 0x7f || *b == b'"' || *b == b'\\');
    if needs_quote {
        out.push(b'"');
        for b in path.iter().copied() {
            match b {
                0x07 => out.extend_from_slice(b"\\a"),
                0x08 => out.extend_from_slice(b"\\b"),
                b'\t' => out.extend_from_slice(b"\\t"),
                b'\n' => out.extend_from_slice(b"\\n"),
                0x0b => out.extend_from_slice(b"\\v"),
                0x0c => out.extend_from_slice(b"\\f"),
                b'\r' => out.extend_from_slice(b"\\r"),
                b'"' => out.extend_from_slice(b"\\\""),
                b'\\' => out.extend_from_slice(b"\\\\"),
                b if b < 0x20 || b >= 0x7f => {
                    out.extend_from_slice(format!("\\{b:03o}").as_bytes());
                }
                b => out.push(b),
            }
        }
        out.push(b'"');
    } else if path.contains(&b' ') {
        out.push(b'"');
        out.extend_from_slice(path);
        out.push(b'"');
    } else {
        out.extend_from_slice(path);
    }
}
