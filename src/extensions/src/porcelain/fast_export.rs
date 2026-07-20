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
//! ### Argument handling
//!
//! git processes a `fast-export` command line in a fixed order, and the exit
//! code depends on which stage rejects it. This module reproduces that order:
//!
//! 1. no arguments at all → the option usage on stderr, exit 129
//! 2. `setup_revisions` → an unresolvable revision is
//!    `fatal: ambiguous argument ...`, exit 128
//! 3. leftover/unknown arguments → the option usage on stderr, exit 129
//! 4. `--anonymize-map` without `--anonymize` → fatal, exit 128
//! 5. `--ancestry-path` with no negative revision → fatal, exit 128
//!
//! ### Covered (byte-identical stdout, exit code and marks file against stock git)
//!
//! * `fast-export --all`, `--branches`, `--tags`, `--remotes`, `--reflog`
//! * `<rev>...`, `<a>..<b>`, `<a>...<b>`, `^<rev>`, `--not`
//! * blob / commit / `reset` / lightweight-tag / annotated-tag stanzas, including
//!   the trailing `reset`+`tag` block emitted in git's reverse-sorted ref order,
//!   with `from <null-oid>` for refs whose commit was excluded
//! * `--no-data`, `--data`, `--full-tree`, `--use-done-feature`,
//!   `--show-original-ids`, `--mark-tags`, `--progress=<n>`, `--export-marks=<file>`
//! * `--signed-tags=(verbatim|warn|warn-verbatim|warn-strip|strip|abort)`
//! * `--tag-of-filtered-object=(abort|drop)`
//! * `--signed-commits=(strip|warn-strip|abort)`, `--reencode=(no|abort)`; the
//!   two `abort` modes reproduce git's `die()` message and exit 128, and all
//!   modes are accepted at parse time since most commits trigger none of them
//! * `--anonymize`
//! * rev-list limiting: `--max-count=<n>`, `--skip=<n>`, `--no-merges`,
//!   `--merges`, `--first-parent`, `--topo-order`, `--date-order`, `--reverse`
//! * accepted no-ops (as in git for a pathspec-less export): `--full-history`,
//!   `--simplify-merges`, `--sparse`, `--dense`, `--boundary` (without negative
//!   revisions)
//!
//! ### Honest limitations (bailed on with a precise message, never silently ignored)
//!
//! * `-M`/`-C` — rename/copy detection needs `diffcore-rename`, which the
//!   vendored `gix-diff` does not expose in a form that reproduces git's
//!   `R`/`C` stanzas. Accepted, and bailed on only when a commit is actually
//!   diffed against an exported parent, where renames could be detected.
//! * `--anonymize-map=<from>[:<to>]` — git's mapping interacts with its token
//!   generator in ways not reproducible from here.
//! * `--anonymize` combined with `--no-data`, `--show-original-ids`, or a
//!   gitlink entry — those emit object ids, which git replaces with generated
//!   fake ids this port does not reproduce.
//! * `--signed-commits=(verbatim|warn-verbatim)` on a signed commit — emitting
//!   `gpgsig` stanzas requires the experimental signed-commit stream extension.
//! * `--reencode=yes` on a commit carrying an `encoding` header — needs iconv;
//!   no re-encoding substrate is vendored.
//! * `--tag-of-filtered-object=rewrite` on a filtered tag — needs rev-list
//!   parent rewriting.
//! * `--import-marks=<file>`, `--import-marks-if-exists=<file>`,
//!   `--reference-excluded-parents`, `--refspec=<refspec>` (with refs to export),
//!   `--ancestry-path` (with negative revisions), `--boundary` (with negative
//!   revisions), and pathspec filtering (`-- <path>...`).

use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::tree::{EntryKind, EntryMode};

/// git's `fast_export_usage` block, byte-for-byte, including the trailing blank
/// line. Printed to stderr for both "no arguments" and "leftover arguments".
const USAGE: &str = "\
usage: git fast-export [<rev-list-opts>]

    --[no-]progress <n>   show progress after <n> objects
    --[no-]signed-tags <mode>
                          select handling of signed tags
    --[no-]signed-commits <mode>
                          select handling of signed commits
    --[no-]tag-of-filtered-object <mode>
                          select handling of tags that tag filtered objects
    --[no-]reencode <mode>
                          select handling of commit messages in an alternate encoding
    --[no-]export-marks <file>
                          dump marks to this file
    --[no-]import-marks <file>
                          import marks from this file
    --[no-]import-marks-if-exists <file>
                          import marks from this file if it exists
    --[no-]fake-missing-tagger
                          fake a tagger when tags lack one
    --[no-]full-tree      output full tree for each commit
    --[no-]use-done-feature
                          use the done feature to terminate the stream
    --no-data             skip output of blob data
    --data                opposite of --no-data
    --[no-]refspec <refspec>
                          apply refspec to exported refs
    --[no-]anonymize      anonymize output
    --anonymize-map <from:to>
                          convert <from> to <to> in anonymized output
    --[no-]reference-excluded-parents
                          reference parents which are not in fast-export stream by object id
    --[no-]show-original-ids
                          show original object ids of blobs/commits
    --[no-]mark-tags      label tags with mark ids

";

/// git's `usage_with_options`: the option list on stderr, exit 129.
fn usage_exit() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// git's `die()` for a revision that neither resolves nor names a path.
fn fatal_ambiguous(arg: &str) -> ExitCode {
    eprint!(
        "fatal: ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'\n"
    );
    ExitCode::from(128)
}

/// git's `die()`: `fatal: <msg>` on stderr, exit 128.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// A `die()` reached while the stream is already being produced.
///
/// git writes the stream as it goes, so everything emitted before the failure is
/// still on stdout when it exits; this port buffers, so the buffer is flushed
/// first to keep both streams byte-identical.
struct Fatal(String);

fn die_midstream(out: &[u8], f: &Fatal) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(out);
    let _ = stdout.flush();
    fatal(&f.0)
}

/// How a signature found in a tag (or commit) is dealt with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SignedMode {
    Verbatim,
    WarnVerbatim,
    WarnStrip,
    Strip,
    Abort,
}

impl SignedMode {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "verbatim" => SignedMode::Verbatim,
            "warn" | "warn-verbatim" => SignedMode::WarnVerbatim,
            "warn-strip" => SignedMode::WarnStrip,
            "strip" => SignedMode::Strip,
            "abort" => SignedMode::Abort,
            _ => return None,
        })
    }
}

/// `--tag-of-filtered-object`: what to do with a tag whose object was not exported.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FilteredTagMode {
    Abort,
    Drop,
    Rewrite,
}

/// `--reencode`: what to do with a commit carrying an `encoding` header.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReencodeMode {
    Yes,
    No,
    Abort,
}

/// The traversal order; git's `--topo-order` (fast-export's default) or `--date-order`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    Topo,
    Date,
}

/// Parsed command-line options for a single `fast-export` invocation.
struct Opts {
    no_data: bool,           // --no-data / --data: refer to blobs by hash
    full_tree: bool,         // --full-tree: `deleteall` plus the whole tree per commit
    use_done: bool,          // --use-done-feature: `feature done` header and `done` trailer
    show_original_ids: bool, // --show-original-ids: `original-oid <sha>` directives
    mark_tags: bool,         // --mark-tags: give annotated tags a mark too
    fake_missing_tagger: bool, // --fake-missing-tagger
    progress: Option<u64>,   // --progress=<n>: a `progress` line every <n> objects
    export_marks: Option<String>, // --export-marks=<file>
    signed_tags: SignedMode, // --signed-tags=<mode>
    signed_commits: SignedMode, // --signed-commits=<mode>
    filtered_tag: FilteredTagMode, // --tag-of-filtered-object=<mode>
    reencode: ReencodeMode,  // --reencode=<mode>
    rename_detection: bool,  // -M / -C
    anonymize: bool,         // --anonymize
}

/// The tagger git invents for a tag object that has none, when asked to.
const FAKE_TAGGER: &str = "tagger <unknown> <unknown> 0 +0000";

/// git's `null_oid()` as printed in a `reset` for an excluded commit.
const NULL_OID: &str = "0000000000000000000000000000000000000000";

/// `git fast-export` — see the module documentation for the covered surface.
pub fn fast_export(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first().map(String::as_str) {
        Some("fast-export") => &args[1..],
        _ => args,
    };

    // git: `if (argc == 1) usage_with_options(...)` — a bare `fast-export` is a
    // usage error, while any single option makes it a (possibly empty) export.
    if args.is_empty() {
        return Ok(usage_exit());
    }

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
        signed_commits: SignedMode::Strip,
        filtered_tag: FilteredTagMode::Abort,
        reencode: ReencodeMode::Abort,
        rename_detection: false,
        anonymize: false,
    };

    // Revision selection, in command-line order so `--not` scopes correctly.
    let mut rev_tokens: Vec<(String, bool)> = Vec::new();
    let mut negate_rest = false;
    let (mut use_all, mut use_branches, mut use_tags, mut use_remotes, mut use_reflog) =
        (false, false, false, false, false);

    // rev-list limiting.
    let mut order = Order::Topo;
    let mut first_parent = false;
    let mut no_merges = false;
    let mut only_merges = false;
    let mut max_count: Option<usize> = None;
    let mut skip: usize = 0;
    let mut ancestry_path = false;
    let mut boundary = false;

    // Deferred diagnostics — git reports these only after the revision walk has
    // been set up, so the order of checks below has to match.
    let mut leftover = false;
    let mut anonymize_map: Vec<String> = Vec::new();
    let mut import_marks: Option<String> = None;
    let mut refspecs: Vec<String> = Vec::new();
    let mut reference_excluded_parents = false;
    let mut pathspecs: Vec<String> = Vec::new();
    let mut in_pathspecs = false;

    for a in args {
        let s = a.as_str();
        if in_pathspecs {
            pathspecs.push(s.to_string());
            continue;
        }
        match s {
            "--" => in_pathspecs = true,

            // ---- fast-export's own options ----
            "--no-data" => opts.no_data = true,
            "--data" => opts.no_data = false,
            "--full-tree" => opts.full_tree = true,
            "--use-done-feature" => opts.use_done = true,
            "--show-original-ids" => opts.show_original_ids = true,
            "--mark-tags" => opts.mark_tags = true,
            "--fake-missing-tagger" => opts.fake_missing_tagger = true,
            "--anonymize" => opts.anonymize = true,
            "--reference-excluded-parents" => reference_excluded_parents = true,

            // ---- rev-list selection ----
            "--all" => use_all = true,
            "--branches" => use_branches = true,
            "--tags" => use_tags = true,
            "--remotes" => use_remotes = true,
            "--reflog" => use_reflog = true,
            "--not" => negate_rest = true,

            // ---- rev-list ordering and limiting ----
            "--topo-order" => order = Order::Topo,
            "--date-order" | "--author-date-order" => order = Order::Date,
            // fast-export sets `revs.reverse` itself after parsing, so an
            // explicit `--reverse` on the command line has no effect.
            "--reverse" => {}
            "--first-parent" => first_parent = true,
            "--no-merges" => no_merges = true,
            "--merges" => only_merges = true,
            "--ancestry-path" => ancestry_path = true,
            "--boundary" => boundary = true,
            // History simplification without a pathspec leaves the commit set
            // untouched, which is the only way fast-export can be invoked here.
            "--full-history" | "--simplify-merges" | "--sparse" | "--dense" => {}
            // Rename/copy detection: recorded, and rejected only where it could
            // actually change the stream (see `emit_commit`).
            "-M" | "-C" => opts.rename_detection = true,

            _ if s.starts_with("--progress=") => {
                let v = &s["--progress=".len()..];
                match v.parse::<u64>() {
                    Ok(n) => opts.progress = Some(n),
                    Err(_) => return Ok(usage_exit()),
                }
            }
            _ if s.starts_with("--max-count=") => {
                match s["--max-count=".len()..].parse::<usize>() {
                    Ok(n) => max_count = Some(n),
                    Err(_) => return Ok(usage_exit()),
                }
            }
            _ if s.starts_with("--skip=") => match s["--skip=".len()..].parse::<usize>() {
                Ok(n) => skip = n,
                Err(_) => return Ok(usage_exit()),
            },
            _ if s.starts_with("--export-marks=") => {
                opts.export_marks = Some(s["--export-marks=".len()..].to_string());
            }
            _ if s.starts_with("--import-marks=") => {
                import_marks = Some(s["--import-marks=".len()..].to_string());
            }
            _ if s.starts_with("--import-marks-if-exists=") => {
                import_marks = Some(s["--import-marks-if-exists=".len()..].to_string());
            }
            _ if s.starts_with("--refspec=") => {
                refspecs.push(s["--refspec=".len()..].to_string());
            }
            _ if s.starts_with("--anonymize-map=") => {
                anonymize_map.push(s["--anonymize-map=".len()..].to_string());
            }
            _ if s.starts_with("--signed-tags=") => {
                match SignedMode::parse(&s["--signed-tags=".len()..]) {
                    Some(m) => opts.signed_tags = m,
                    None => return Ok(usage_exit()),
                }
            }
            _ if s.starts_with("--signed-commits=") => {
                match SignedMode::parse(&s["--signed-commits=".len()..]) {
                    Some(m) => opts.signed_commits = m,
                    None => return Ok(usage_exit()),
                }
            }
            _ if s.starts_with("--tag-of-filtered-object=") => {
                opts.filtered_tag = match &s["--tag-of-filtered-object=".len()..] {
                    "abort" => FilteredTagMode::Abort,
                    "drop" => FilteredTagMode::Drop,
                    "rewrite" => FilteredTagMode::Rewrite,
                    _ => return Ok(usage_exit()),
                };
            }
            _ if s.starts_with("--reencode=") => {
                opts.reencode = match &s["--reencode=".len()..] {
                    "yes" => ReencodeMode::Yes,
                    "no" => ReencodeMode::No,
                    "abort" => ReencodeMode::Abort,
                    _ => return Ok(usage_exit()),
                };
            }

            // Anything else beginning with `-` survives both option parsers and
            // ends up as a leftover argument, which git turns into a usage error.
            _ if s.starts_with('-') && s != "-" => leftover = true,

            _ => rev_tokens.push((s.to_string(), negate_rest)),
        }
    }

    let repo = gix::discover(".")?;

    // ---- Stage 2: `setup_revisions` resolves revisions and dies on the first bad one. ----
    let mut sel = Selection::default();
    for (tok, negated) in &rev_tokens {
        if add_rev_token(&repo, tok, *negated, &mut sel).is_err() {
            return Ok(fatal_ambiguous(tok));
        }
    }

    // ---- Stage 3: leftover arguments. ----
    if leftover {
        return Ok(usage_exit());
    }

    // ---- Stage 4/5: the two late fatals, in git's order. ----
    if !anonymize_map.is_empty() && !opts.anonymize {
        return Ok(fatal("the option '--anonymize-map' requires '--anonymize'"));
    }
    if ancestry_path && sel.hidden.is_empty() {
        return Ok(fatal("--ancestry-path given but there are no bottom commits"));
    }

    // ---- Options this port does not implement: refuse rather than mis-export. ----
    if import_marks.is_some() {
        bail!("--import-marks is not supported");
    }
    if reference_excluded_parents {
        bail!("--reference-excluded-parents is not supported");
    }
    if !pathspecs.is_empty() {
        bail!("pathspec filtering is not supported");
    }
    if ancestry_path {
        bail!("--ancestry-path is not supported");
    }
    if boundary && !sel.hidden.is_empty() {
        bail!("--boundary with negative revisions is not supported");
    }
    if !anonymize_map.is_empty() {
        bail!("--anonymize-map is not supported");
    }
    if opts.anonymize && (opts.no_data || opts.show_original_ids) {
        bail!("--anonymize with --no-data or --show-original-ids is not supported");
    }

    // ---- Ref selection (`--all` and friends), in git's iteration order. ----
    let mut cmdline: Vec<(BString, ObjectId)> = Vec::new();
    if use_all || use_branches || use_tags || use_remotes {
        let mut names: Vec<BString> = Vec::new();
        for reference in repo.references()?.all()? {
            let reference = reference.map_err(|e| anyhow!("{e}"))?;
            let name = reference.name().as_bstr().to_owned();
            let keep = use_all
                || (use_branches && name.starts_with(b"refs/heads/"))
                || (use_tags && name.starts_with(b"refs/tags/"))
                || (use_remotes && name.starts_with(b"refs/remotes/"));
            if keep {
                names.push(name);
            }
        }
        names.sort();
        for name in names {
            let spec = name.to_str().map_err(|_| anyhow!("non-UTF-8 ref {name:?}"))?;
            let id = repo.rev_parse_single(spec)?.detach();
            cmdline.push((name, id));
        }
        // git's `--all` also feeds `head_ref`, which only contributes a distinct
        // entry when HEAD is detached; otherwise it resolves to a ref already listed.
        if use_all && repo.head()?.is_detached() {
            if let Ok(id) = repo.rev_parse_single("HEAD") {
                cmdline.push((BString::from("HEAD"), id.detach()));
            }
        }
    }
    for (name, target) in &sel.named {
        cmdline.push((name.clone(), *target));
    }

    if !refspecs.is_empty() && !cmdline.is_empty() {
        bail!("--refspec is not supported");
    }

    // `--reflog` contributes tips with no name at all; git prints an empty
    // refname for commits it reaches only that way.
    let mut tips: Vec<ObjectId> = sel.tips.clone();
    if use_reflog {
        collect_reflog_tips(&repo, &mut tips)?;
    }

    // ---- Ref bookkeeping, mirroring `get_tags_and_duplicates`. ----
    // `sources` is git's `--source` decoration: the ref name a commit is printed
    // under. The first cmdline ref reaching a commit wins; later ones become
    // standalone `reset` stanzas. Annotated tags claim a source too, but never
    // produce a duplicate `reset`.
    let mut sources: HashMap<ObjectId, BString> = HashMap::new();
    // Every commit-valued cmdline ref, whether or not it ends up labelling a
    // commit. git's comment on this list: "make sure this ref gets properly
    // updated eventually, whether through a commit or manually at the end".
    let mut commit_refs: Vec<(BString, ObjectId)> = Vec::new();
    let mut tag_refs: Vec<(BString, ObjectId)> = Vec::new();

    for (name, target) in &cmdline {
        let object = repo.find_object(*target)?;
        let is_tag = object.kind == gix::object::Kind::Tag;
        let Ok(commit) = object.peel_to_commit() else {
            continue; // a ref to a blob or tree is not exportable, as in git
        };
        let commit_id = commit.id;
        if is_tag {
            tag_refs.push((name.clone(), *target));
        } else {
            commit_refs.push((name.clone(), commit_id));
        }
        sources.entry(commit_id).or_insert_with(|| name.clone());
        tips.push(commit_id);
    }
    tips.sort();
    tips.dedup();

    let hidden = sel.hidden.clone();

    // ---- Source propagation over the commit-date walk git uses for it. ----
    if !tips.is_empty() {
        let mut platform = repo
            .rev_walk(tips.clone())
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ));
        if first_parent {
            platform = platform.first_parent_only();
        }
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

    // ---- Emission order: `rev-list [--topo-order|--date-order] --reverse`. ----
    let mut order_list: Vec<gix::traverse::commit::Info> = Vec::new();
    if !tips.is_empty() {
        let topo = gix::traverse::commit::topo::Builder::from_iters(
            &repo.objects,
            tips.clone(),
            Some(hidden.clone()),
        )
        .sorting(match order {
            Order::Topo => gix::traverse::commit::topo::Sorting::TopoOrder,
            Order::Date => gix::traverse::commit::topo::Sorting::DateOrder,
        })
        .parents(if first_parent {
            gix::traverse::commit::Parents::First
        } else {
            gix::traverse::commit::Parents::All
        })
        .build()?;
        for info in topo {
            order_list.push(info?);
        }
    }

    // git applies `commit_ignore` (`--no-merges`/`--merges`), then `--skip`, then
    // `--max-count`, all in rev-list order — before fast-export reverses.
    if no_merges {
        order_list.retain(|i| i.parent_ids.len() <= 1);
    }
    if only_merges {
        order_list.retain(|i| i.parent_ids.len() > 1);
    }
    if skip > 0 {
        order_list.drain(..skip.min(order_list.len()));
    }
    if let Some(n) = max_count {
        order_list.truncate(n);
    }
    order_list.reverse();

    let mut st = State {
        out: Vec::new(),
        marks: HashMap::new(),
        commit_marks: Vec::new(),
        last_mark: 0,
        counter: 0,
        labels: std::collections::HashSet::new(),
        anon: Anon::default(),
    };

    if opts.use_done {
        st.out.extend_from_slice(b"feature done\n");
    }

    for info in &order_list {
        if let Some(f) = emit_commit(&repo, info, &opts, &sources, &mut st)? {
            return Ok(die_midstream(&st.out, &f));
        }
    }

    // ---- Trailing `reset`/`tag` block. ----
    // A cmdline ref that never appeared as a commit label still has to be
    // pointed somewhere, so git emits a `reset` for it: at the mark of the
    // commit it names, or at the null oid when that commit was not exported.
    // The list is sorted by ref name and walked backwards.
    let mut trailing: Vec<(BString, ObjectId)> = commit_refs
        .into_iter()
        .filter(|(name, _)| !st.labels.contains(name))
        .collect();
    trailing.sort();
    trailing.dedup();
    for (name, commit_id) in trailing.iter().rev() {
        let printed = st.anon_refname(&opts, name.as_bstr());
        let mark = st.marks.get(commit_id).copied();
        st.out.extend_from_slice(b"reset ");
        st.out.extend_from_slice(&printed);
        match mark {
            Some(mark) => st
                .out
                .extend_from_slice(format!("\nfrom :{mark}\n\n").as_bytes()),
            // The commit was excluded from this export; git points the ref at the
            // null oid, which fast-import reads as "delete this branch".
            None => st
                .out
                .extend_from_slice(format!("\nfrom {NULL_OID}\n\n").as_bytes()),
        }
    }

    tag_refs.sort();
    for (name, tag_id) in tag_refs.iter().rev() {
        if let Some(f) = emit_tag(&repo, name.as_bstr(), *tag_id, &opts, &mut st)? {
            return Ok(die_midstream(&st.out, &f));
        }
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

// ---------------------------------------------------------------------------
// Revision selection
// ---------------------------------------------------------------------------

/// What the positional arguments selected: named cmdline refs, positive tips,
/// and the negative (`^rev`, left side of a range) commits.
#[derive(Default)]
struct Selection {
    named: Vec<(BString, ObjectId)>,
    tips: Vec<ObjectId>,
    hidden: Vec<ObjectId>,
}

/// Resolve one positional revision argument, in git's `handle_revision_arg` shape.
///
/// `Err(())` means the whole argument is neither a revision nor a path, which the
/// caller turns into git's `ambiguous argument` fatal.
fn add_rev_token(
    repo: &gix::Repository,
    tok: &str,
    negated: bool,
    sel: &mut Selection,
) -> std::result::Result<(), ()> {
    if tok.is_empty() {
        return Err(());
    }
    if let Some(rest) = tok.strip_prefix('^') {
        let id = commit_of(repo, rest).ok_or(())?;
        sel.hidden.push(id);
        return Ok(());
    }
    if let Some((l, r)) = tok.split_once("...") {
        let (l, r) = (default_head(l), default_head(r));
        let (lc, rc) = (commit_of(repo, l).ok_or(())?, commit_of(repo, r).ok_or(())?);
        // `A...B` is `A B --not $(git merge-base --all A B)`.
        for base in repo.merge_bases_many(lc, &[rc]).map_err(|_| ())? {
            sel.hidden.push(base.detach());
        }
        add_positive(repo, l, lc, sel);
        add_positive(repo, r, rc, sel);
        return Ok(());
    }
    if let Some((l, r)) = tok.split_once("..") {
        let (l, r) = (default_head(l), default_head(r));
        sel.hidden.push(commit_of(repo, l).ok_or(())?);
        let rc = commit_of(repo, r).ok_or(())?;
        add_positive(repo, r, rc, sel);
        return Ok(());
    }
    let id = commit_of(repo, tok).ok_or(())?;
    if negated {
        sel.hidden.push(id);
    } else {
        add_positive(repo, tok, id, sel);
    }
    Ok(())
}

/// An omitted range endpoint means `HEAD`, as in `..main` or `main..`.
fn default_head(s: &str) -> &str {
    if s.is_empty() { "HEAD" } else { s }
}

/// Record a positive tip, plus a cmdline ref entry when the spec dwims to a ref.
///
/// git's `get_tags_and_duplicates` only names arguments that `dwim_ref` resolves;
/// a raw commit id contributes history but no label.
fn add_positive(repo: &gix::Repository, spec: &str, commit_id: ObjectId, sel: &mut Selection) {
    sel.tips.push(commit_id);
    if let Some((name, target)) = dwim_ref(repo, spec) {
        sel.named.push((name, target));
    }
}

/// Resolve a revision to the id of the commit it names, peeling tags.
fn commit_of(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    Some(
        repo.rev_parse_single(spec)
            .ok()?
            .object()
            .ok()?
            .peel_to_commit()
            .ok()?
            .id,
    )
}

/// git's `repo_dwim_ref`: the fully-resolved ref name a spec names, if any.
///
/// Symrefs are followed to their final target, which is why `HEAD` on an attached
/// worktree labels commits `refs/heads/<branch>` rather than `HEAD`.
fn dwim_ref(repo: &gix::Repository, spec: &str) -> Option<(BString, ObjectId)> {
    let mut reference = repo.try_find_reference(spec).ok().flatten()?;
    while let Some(Ok(next)) = reference.follow() {
        reference = next;
    }
    let name = reference.name().as_bstr().to_owned();
    let target = match reference.target() {
        gix::refs::TargetRef::Object(id) => id.to_owned(),
        gix::refs::TargetRef::Symbolic(_) => return None,
    };
    Some((name, target))
}

/// git's `add_reflogs_to_pending`: every object a reflog ever pointed at becomes
/// an unnamed tip.
fn collect_reflog_tips(repo: &gix::Repository, tips: &mut Vec<ObjectId>) -> Result<()> {
    let mut refs: Vec<gix::Reference<'_>> = Vec::new();
    if let Ok(head) = repo.find_reference("HEAD") {
        refs.push(head);
    }
    let platform = repo.references()?;
    for reference in platform.all()?.flatten() {
        refs.push(reference);
    }
    for reference in &refs {
        let mut platform = reference.log_iter();
        let Ok(Some(iter)) = platform.all() else {
            continue;
        };
        for line in iter {
            let Ok(line) = line else { continue };
            for id in [line.previous_oid(), line.new_oid()] {
                if id.is_null() {
                    continue;
                }
                if let Some(commit) = repo
                    .find_object(id)
                    .ok()
                    .and_then(|o| o.peel_to_commit().ok())
                {
                    tips.push(commit.id);
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Anonymization (`--anonymize`)
// ---------------------------------------------------------------------------

/// git's anonymization tables: one generated token per distinct input, handed out
/// in the order the stream first mentions it.
#[derive(Default)]
struct Anon {
    refs: HashMap<BString, BString>,
    paths: HashMap<BString, BString>,
    idents: HashMap<BString, BString>,
    blob_counter: u32,
    message_counter: u32,
    tag_message_counter: u32,
}

impl Anon {
    /// git's `anonymize_refname`: the `refs/heads/`-style prefix survives, every
    /// remaining component becomes `ref<n>` from one shared counter.
    fn refname(&mut self, name: &BStr) -> BString {
        const PREFIXES: [&[u8]; 4] = [
            b"refs/heads/".as_slice(),
            b"refs/tags/".as_slice(),
            b"refs/remotes/".as_slice(),
            b"refs/".as_slice(),
        ];
        let raw: &[u8] = name;
        let mut out = BString::default();
        let mut rest = raw;
        for p in PREFIXES {
            if let Some(r) = raw.strip_prefix(p) {
                out.extend_from_slice(p);
                rest = r;
                break;
            }
        }
        Self::map_components(&mut self.refs, rest, "ref", &mut out);
        out
    }

    /// git's `anonymize_path`: each `/`-separated component is mapped on its own,
    /// so shared directories keep sharing a generated name.
    fn path(&mut self, path: &BStr) -> BString {
        let mut out = BString::default();
        Self::map_components(&mut self.paths, path, "path", &mut out);
        out
    }

    /// Rewrite every `/`-separated component through `table`, minting
    /// `<prefix><n>` for components never seen before. `n` is the table size, so
    /// tokens are handed out in first-mention order across the whole stream.
    fn map_components(
        table: &mut HashMap<BString, BString>,
        input: &[u8],
        prefix: &str,
        out: &mut BString,
    ) {
        for (i, component) in input.split(|b| *b == b'/').enumerate() {
            if i > 0 {
                out.push(b'/');
            }
            let key = BString::from(component.to_vec());
            if !table.contains_key(&key) {
                let value = BString::from(format!("{prefix}{}", table.len()));
                table.insert(key.clone(), value);
            }
            out.extend_from_slice(&table[&key]);
        }
    }

    /// `<name> <<email>>` becomes `User <n> <user<n>@example.com>`; the timestamp
    /// is left alone, as git does.
    fn ident(&mut self, ident: &[u8]) -> BString {
        let key = BString::from(ident.to_vec());
        let next = self.idents.len();
        self.idents
            .entry(key)
            .or_insert_with(|| BString::from(format!("User {next} <user{next}@example.com>")))
            .clone()
    }

    fn blob(&mut self) -> Vec<u8> {
        let n = self.blob_counter;
        self.blob_counter += 1;
        format!("anonymous blob {n}").into_bytes()
    }

    fn message(&mut self) -> Vec<u8> {
        let n = self.message_counter;
        self.message_counter += 1;
        format!("subject {n}\n\nbody\n").into_bytes()
    }

    fn tag_message(&mut self) -> Vec<u8> {
        let n = self.tag_message_counter;
        self.tag_message_counter += 1;
        format!("tag message {n}").into_bytes()
    }
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
    /// Every ref name that has appeared as a `commit`/`reset` label, before
    /// anonymization. A cmdline ref missing from this set needs a trailing
    /// `reset` so the importer still updates it.
    labels: std::collections::HashSet<BString>,
    anon: Anon,
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

    /// The ref name as it should appear in the stream.
    fn anon_refname(&mut self, opts: &Opts, name: &BStr) -> BString {
        if opts.anonymize {
            self.anon.refname(name)
        } else {
            name.to_owned()
        }
    }

    /// The `author`/`committer`/`tagger` line as it should appear in the stream.
    fn anon_ident_line(&mut self, opts: &Opts, line: &[u8]) -> Vec<u8> {
        if !opts.anonymize {
            return line.to_vec();
        }
        // `<keyword> <name> <<email>> <timestamp> <tz>`
        let Some(kw_end) = line.iter().position(|b| *b == b' ') else {
            return line.to_vec();
        };
        let Some(gt) = line.iter().rposition(|b| *b == b'>') else {
            return line.to_vec();
        };
        let mut out = line[..=kw_end].to_vec();
        out.extend_from_slice(&self.anon.ident(&line[kw_end + 1..=gt]));
        out.extend_from_slice(&line[gt + 1..]);
        out
    }
}

/// Emit one commit: its new blobs first, then the `commit` stanza.
fn emit_commit(
    repo: &gix::Repository,
    info: &gix::traverse::commit::Info,
    opts: &Opts,
    sources: &HashMap<ObjectId, BString>,
    st: &mut State,
) -> Result<Option<Fatal>> {
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

    // `--reencode` only has anything to decide when the commit declares its own
    // encoding. `no` keeps the header as-is, which is what this port does; the
    // other modes either die (`abort`) or need iconv (`yes`).
    if let Some(encoding) = header_value(headers, b"encoding") {
        match opts.reencode {
            ReencodeMode::No => {}
            ReencodeMode::Abort => {
                let encoding = encoding.to_str_lossy();
                return Ok(Some(Fatal(format!(
                    "encountered commit-specific encoding {encoding} in commit {id}; \
                     use --reencode=[yes|no] to handle it"
                ))));
            }
            ReencodeMode::Yes => bail!("--reencode=yes is not supported (no iconv substrate)"),
        }
    }
    // Likewise for `--signed-commits`: `strip`/`warn-strip` are what dropping the
    // header achieves, `abort` dies, and the rest need the gpgsig stream extension.
    if header_value(headers, b"gpgsig").is_some() {
        match opts.signed_commits {
            SignedMode::Strip => {}
            SignedMode::WarnStrip => {
                eprintln!("warning: stripping signature from commit {id}");
            }
            SignedMode::Abort => {
                return Ok(Some(Fatal(format!(
                    "encountered signed commit {id}; use --signed-commits=<mode> to handle it"
                ))));
            }
            SignedMode::Verbatim | SignedMode::WarnVerbatim => bail!(
                "--signed-commits=(verbatim|warn-verbatim) is not supported \
                 (commit {id} carries a signature)"
            ),
        }
    }

    // git diffs against the first parent only when that parent is itself in the
    // stream; otherwise the commit is emitted as a root against the empty tree.
    let base = if opts.full_tree {
        None
    } else {
        match parents.first() {
            Some(p) if st.marks.contains_key(p) => Some(repo.find_object(*p)?.peel_to_tree()?.id),
            _ => None,
        }
    };
    if base.is_some() && opts.rename_detection {
        bail!("-M/-C rename detection is not supported");
    }
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

    // A commit reached only through `--reflog` has no name; git prints an empty one.
    let source = sources.get(&id).cloned().unwrap_or_default();
    st.labels.insert(source.clone());
    let refname = st.anon_refname(opts, source.as_bstr());

    let mark = st.next_mark(id);
    st.commit_marks.push((mark, id));

    if parents.is_empty() {
        st.out.extend_from_slice(b"reset ");
        st.out.extend_from_slice(&refname);
        st.out.push(b'\n');
    }
    st.out.extend_from_slice(b"commit ");
    st.out.extend_from_slice(&refname);
    st.out
        .extend_from_slice(format!("\nmark :{mark}\n").as_bytes());
    if opts.show_original_ids {
        st.out
            .extend_from_slice(format!("original-oid {id}\n").as_bytes());
    }
    let author = st.anon_ident_line(opts, author);
    st.out.extend_from_slice(&author);
    st.out.push(b'\n');
    let committer = st.anon_ident_line(opts, committer);
    st.out.extend_from_slice(&committer);
    st.out.push(b'\n');
    let message: Vec<u8> = if opts.anonymize {
        st.anon.message()
    } else {
        message.to_vec()
    };
    st.out
        .extend_from_slice(format!("data {}\n", message.len()).as_bytes());
    st.out.extend_from_slice(&message);

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
        render_change(c, opts, st)?;
    }
    st.out.push(b'\n');
    st.tick(opts);
    Ok(None)
}

/// git's `export_blob`: a `blob` stanza, once per distinct object.
fn emit_blob(repo: &gix::Repository, id: ObjectId, opts: &Opts, st: &mut State) -> Result<()> {
    if st.marks.contains_key(&id) {
        return Ok(());
    }
    let data = if opts.anonymize {
        st.anon.blob()
    } else {
        repo.find_object(id)?.data.clone()
    };
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
) -> Result<Option<Fatal>> {
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
            FilteredTagMode::Drop => Ok(None),
            FilteredTagMode::Abort => Ok(Some(Fatal(format!(
                "tag {tag_id} tags unexported object; \
                 use --tag-of-filtered-object=<mode> to handle it"
            )))),
            FilteredTagMode::Rewrite => bail!(
                "--tag-of-filtered-object=rewrite is not supported \
                 (tag {tag_id} tags an unexported object)"
            ),
        };
    };

    // git looks for the signature block and applies --signed-tags to it.
    if let Some(pos) = find_sub(message, b"\n-----BEGIN PGP SIGNATURE-----\n") {
        match opts.signed_tags {
            SignedMode::Abort => {
                return Ok(Some(Fatal(format!(
                    "encountered signed tag {tag_id}; use --signed-tags=<mode> to handle it"
                ))));
            }
            SignedMode::WarnVerbatim => eprintln!("warning: exporting signed tag {tag_id}"),
            SignedMode::Verbatim => {}
            SignedMode::WarnStrip => {
                eprintln!("warning: stripping signature from tag {tag_id}");
                message = &message[..pos + 1];
            }
            SignedMode::Strip => message = &message[..pos + 1],
        }
    }

    let printed_name = st.anon_refname(opts, full_name);
    let full: &[u8] = &printed_name;
    let short = full.strip_prefix(&b"refs/tags/"[..]).unwrap_or(full).to_vec();
    st.out.extend_from_slice(b"tag ");
    st.out.extend_from_slice(&short);
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
            let line = st.anon_ident_line(opts, line);
            st.out.extend_from_slice(&line);
            st.out.push(b'\n');
        }
        None if opts.fake_missing_tagger => {
            st.out.extend_from_slice(FAKE_TAGGER.as_bytes());
            st.out.push(b'\n');
        }
        None => {}
    }
    let message: Vec<u8> = if opts.anonymize {
        st.anon.tag_message()
    } else {
        message.to_vec()
    };
    st.out
        .extend_from_slice(format!("data {}\n", message.len()).as_bytes());
    st.out.extend_from_slice(&message);
    st.out.push(b'\n');
    Ok(None)
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
fn render_change(c: &Change, opts: &Opts, st: &mut State) -> Result<()> {
    let path = if opts.anonymize {
        st.anon.path(c.path.as_bstr())
    } else {
        c.path.clone()
    };
    match c.new {
        None => {
            st.out.extend_from_slice(b"D ");
            print_path(&mut st.out, path.as_bstr());
            st.out.push(b'\n');
        }
        Some(new) => {
            let mode = new.mode.value();
            let reference = if opts.no_data || new.mode.kind() == EntryKind::Commit {
                if opts.anonymize {
                    bail!("--anonymize with gitlink entries is not supported");
                }
                new.id.to_hex().to_string()
            } else {
                let mark = st
                    .marks
                    .get(&new.id)
                    .ok_or_else(|| anyhow!("blob {} was not exported", new.id))?;
                format!(":{mark}")
            };
            st.out
                .extend_from_slice(format!("M {mode:06o} {reference} ").as_bytes());
            print_path(&mut st.out, path.as_bstr());
            st.out.push(b'\n');
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
