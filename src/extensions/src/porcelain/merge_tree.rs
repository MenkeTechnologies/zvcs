//! `git merge-tree` — perform a merge without touching the index or worktree.
//!
//! Only the modern `--write-tree` mode is served. The merge itself is done by
//! the vendored `gix-merge` tree/commit merge, which performs the same class of
//! work git's `merge-ort` does: three-way content merges, rename detection and
//! recursive merge-base consolidation.
//!
//! Covered, byte-for-byte against stock git:
//!   * clean merges — the merged tree id and nothing else, exit 0
//!   * conflicted merges — tree id, the `<mode> <object> <stage>\t<path>` stage
//!     lines (or `--name-only` paths), and the informational-message block,
//!     exit 1
//!   * `-z`, `--name-only`, `--messages`/`--no-messages`, `--quiet`,
//!     `--allow-unrelated-histories`, `--merge-base=<tree-ish>`,
//!     `--write-tree` (the default mode), `--`
//!   * the whole option grammar, including every `-X`/`--strategy-option`
//!     spelling git accepts, `--no-strategy-option`, `--trivial-merge`'s
//!     "incompatible with all other options" rule, the `--quiet` +
//!     `--messages` mutual-exclusion `die()` (exit 128, checked before operand
//!     count), and git's usage/`error:` diagnostics with their 128/129 exit
//!     codes
//!   * the strategy options `ours`, `theirs`, `no-renames`, `find-renames`,
//!     `find-renames=<n>`, `rename-threshold=<n>`, `histogram`,
//!     `diff-algorithm=myers|default|minimal|histogram`, `subtree[=<path>]`,
//!     `ignore-space-change`, `ignore-all-space`, `ignore-space-at-eol`,
//!     `ignore-cr-at-eol`, and the no-op `no-renormalize`
//!
//! Also covered:
//!   * `--stdin` — the multi-merge batch protocol: each input line is one merge
//!     (`<branch1> <branch2>` or `<base> -- <branch1> <branch2>`), and each
//!     result is emitted as git's `<clean>\0<tree>\0<-z body>\0` record, with
//!     the same fatal diagnostics (`malformed input line`, `not something we
//!     can merge`, `refusing to merge unrelated histories`) and their exit codes
//!   * the `--quiet` mutual-exclusions with `--name-only`, `--stdin` and `-z`
//!     (each a `die()`, exit 128), and `--stdin`'s exclusion of `--trivial-merge`
//!     and `--merge-base`
//!   * conflict message rendering beyond the plain content family: binary
//!     content merges (`warning: Cannot merge binary files: <p> (<a> vs. <b>)`),
//!     symlink content conflicts, `modify/delete`, `rename/delete` and
//!     `rename/rename` — the side labels are recovered from tree membership so
//!     they track git's argument labels regardless of `gix-merge`'s canonical
//!     side ordering
//!
//! The deprecated `--trivial-merge` mode is ported too: the three-tree
//! lock-step walk (`threeway_callback()` / `resolve()` / `unresolved()`), the
//! `merged`/`added in …`/`changed in both`/`removed in …` stage listing, and the
//! bare unified diff (context 3, no file headers) from our version of each path
//! to the `merge_blobs()` result. Its two operand diagnostics — `unknown rev`
//! and `unable to read tree` — still fire before the walk.
//!
//! `-Xsubtree[=<path>]` shifts *their* tree and the merge base onto our shape
//! through [`crate::merge_apply::shift_tree_object`], the `match-trees.c` port
//! `git merge` already drives, and does it where merge-ort does — in
//! `merge_ort_nonrecursive_internal()`, once the merge base is settled, which is
//! why the base is materialized here rather than left to `merge_commits()`.
//!
//! `-Xignore-*` reaches the blob merge as `xpp.flags`' whitespace rule, spelled
//! as the canonical form `xdl_recmatch()` groups records by
//! ([`super::diff::normalize_line`], shared with the diff family) and handed to
//! `gix-merge`'s text driver, which interns one representative per class while
//! still writing the original bytes.
//!
//! Not covered, and refused rather than approximated:
//!   * the strategy option `renormalize` — `gix-merge`'s blob pipeline is not
//!     driven in renormalizing mode here. It parses and validates exactly as git
//!     does; only performing such a merge is refused.
//!   * message rendering for the two conflict classes [`crate::merge_msg`]
//!     still cannot name: a gitlink content merge, whose git text comes from
//!     `merge_submodule()` plus the `advice.submoduleMergeConflict` hint block,
//!     and the *directory rename* class, whose `CONFLICT (file location)` text
//!     `gix-merge` gives no input to build. (The other half of a D/F conflict —
//!     a blob one side edited and the other replaced with a directory — is
//!     rendered: `gix-merge` reports it as its own resolution failure carrying
//!     the `~<label>` path git moves the blob to, and
//!     [`crate::merge_msg`] prints git's `CONFLICT (file/directory)` line for
//!     it.)
//!     `merge-tree` is the strict half of that renderer: its stdout is a
//!     machine-readable record and nothing has been written when the messages
//!     are produced, so an unrenderable class is refused rather than
//!     approximated. Those merges still work under `--no-messages` and
//!     `--quiet`, where no message text is emitted at all.

use anyhow::Result;
use std::io::{BufRead, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::merge::blob::builtin_driver::text::{
    Conflict as MergeConflict, ConflictStyle, Labels, Level, Merge as TextMerge, Rendering,
};
use gix::merge::tree::apply_index_entries::RemovalMode;
use gix::merge::tree::{FileFavor, TreatAsUnresolved};

/// The outcome of one real (`--write-tree`) merge, ready for framing by the
/// caller. `Fatal` carries the exit code git would `die()`/`exit()` with; it
/// aborts the whole process, including a `--stdin` batch mid-stream.
#[allow(dead_code)] // deliberate port surface; wired by the merge framing path
enum Merged {
    Fatal(ExitCode),
    Done { clean: bool, body: Vec<u8> },
}

/// `cmd_merge_tree()`'s `struct option mt_options[]` (builtin/merge-tree.c), in
/// table order, as [`super::resolve_long`] reads it.
///
/// The two mode selectors and the `OPT_BOOL_F(... PARSE_OPT_NONEG)` flags carry
/// `PARSE_OPT_NONEG`, so only `--messages`, `--merge-base` and
/// `--strategy-option` have a `--no-` spelling.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "write-tree",                  neg: false, arg: super::Arg::None },
    super::LongOpt { name: "trivial-merge",               neg: false, arg: super::Arg::None },
    super::LongOpt { name: "messages",                    neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "quiet",                       neg: false, arg: super::Arg::None },
    super::LongOpt { name: "name-only",                   neg: false, arg: super::Arg::None },
    super::LongOpt { name: "allow-unrelated-histories",   neg: false, arg: super::Arg::None },
    super::LongOpt { name: "stdin",                       neg: false, arg: super::Arg::None },
    super::LongOpt { name: "merge-base",                  neg: true,  arg: super::Arg::Required },
    super::LongOpt { name: "strategy-option",             neg: true,  arg: super::Arg::Required },
];
/// Verbatim `git merge-tree` usage text, printed to stderr for usage errors
/// (git exits 129 in those cases).
const USAGE: &str = "\
usage: git merge-tree [--write-tree] [<options>] <branch1> <branch2>
   or: git merge-tree [--trivial-merge] <base-tree> <branch1> <branch2>

    --write-tree          do a real merge instead of a trivial merge
    --trivial-merge       do a trivial merge only
    --[no-]messages       also show informational/conflict messages
    --quiet               suppress all output; only exit status wanted
    -z                    separate paths with the NUL character
    --name-only           list filenames without modes/oids/stages
    --allow-unrelated-histories
                          allow merging unrelated histories
    --stdin               perform multiple merges, one per line of input
    --[no-]merge-base <tree-ish>
                          specify a merge-base for the merge
    -X, --[no-]strategy-option <option=value>
                          option for selected merge strategy

";

/// git's internal full-similarity score; `-X` rename percentages are expressed
/// as a fraction of it (see `MAX_SCORE` in git's `diffcore.h`).
const MAX_SCORE: u64 = 60000;

// The `XDF_*` whitespace bits of `xdl_opts` (git's `xdiff/xdiff.h`) and the
// `xdl_recmatch()` rules they select, shared with `git merge`'s `-X` path so the
// two commands cannot drift apart.
use crate::merge_ws::{
    XDF_IGNORE_CR_AT_EOL, XDF_IGNORE_WHITESPACE, XDF_IGNORE_WHITESPACE_AT_EOL,
    XDF_IGNORE_WHITESPACE_CHANGE,
};

/// Which of `merge-tree`'s two mutually exclusive modes was requested.
///
/// `Unknown` means neither mode flag was given, in which case git picks the
/// mode from the number of positional arguments.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Unknown,
    Real,
    Trivial,
}

/// The parsed `-X`/`--strategy-option` state, mirroring the subset of git's
/// `struct merge_options` that `parse_merge_opt()` can touch.
#[derive(Default)]
pub(super) struct StrategyOptions {
    /// `ours` / `theirs`.
    favor: Option<FileFavor>,
    /// `subtree` (empty shift) or `subtree=<path>`.
    subtree: Option<String>,
    /// The requested diff algorithm, already normalized to lowercase.
    diff_algorithm: Option<String>,
    /// The `XDF_IGNORE_*` bits the `-Xignore-*` options set in `xdl_opts`.
    ///
    /// `parse_merge_opt()` reaches them through `DIFF_XDL_SET()`, which only ever
    /// *sets* a bit, so the options accumulate and the strongest one decides —
    /// `-Xignore-all-space -Xignore-cr-at-eol` compares the same way
    /// `-Xignore-all-space` alone does, in either order.
    whitespace: u32,
    /// `renormalize` / `no-renormalize`; `None` leaves the configured default.
    renormalize: Option<bool>,
    /// `no-renames` clears this, `find-renames`/`rename-threshold` set it.
    detect_renames: Option<bool>,
    /// A rename score out of [`MAX_SCORE`]; `0` means "git's default".
    rename_score: Option<u32>,
}

/// `git merge-tree [--write-tree] [<options>] <branch1> <branch2>`.
pub fn merge_tree(args: &[String]) -> Result<ExitCode> {
    let mut nul = false;
    let mut name_only = false;
    let mut quiet = false;
    let mut allow_unrelated = false;
    let mut use_stdin = false;
    let mut mode = Mode::Unknown;
    // `None` = git's default (show messages iff the merge is conflicted).
    let mut show_messages: Option<bool> = None;
    let mut merge_base: Option<String> = None;
    let mut xopts: Vec<String> = Vec::new();
    let mut revs: Vec<String> = Vec::new();

    // git remembers how many arguments it started with so that `--trivial-merge`
    // can insist that nothing else was passed alongside it. `dispatch::run` hands
    // us only the post-subcommand argument vector (the `merge-tree` verb is
    // already stripped), so every slot in `args` is a real operand.
    let original_argc = args.len();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        // git's merge-tree parses with PARSE_OPT_STOP_AT_NON_OPTION. A bare `--`
        // ends option parsing and is itself consumed; any other non-option token
        // (including `-`) ends it and is kept. From that point on every remaining
        // argv slot is a positional rev, even one that looks like a flag — e.g.
        // `merge-tree feature -- x` treats both `--` and `x` as revs.
        if a == "--" {
            revs.extend(args[i + 1..].iter().cloned());
            break;
        }
        if !a.starts_with('-') || a == "-" {
            revs.extend(args[i..].iter().cloned());
            break;
        }

        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt(): the name never abbreviates and never takes
        // an `=<value>`, so `--help-a` and `--help-all=x` stay unknown options.
        // This table has no `PARSE_OPT_HIDDEN` entry, so `USAGE_FULL` renders
        // the same block `-h` prints.
        if a == "--help-all" {
            return Ok(super::show_usage(USAGE));
        }

        // Respell a unique abbreviation as the name it resolves to, so `--allow-unre`
        // reaches the same arm as `--allow-unrelated-histories`.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, USAGE))
            }
        };

        if let Some(long) = a.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match name {
                "write-tree" => {
                    if let Some(code) = set_mode(&mut mode, Mode::Real, "--write-tree") {
                        return Ok(code);
                    }
                }
                "trivial-merge" => {
                    if let Some(code) = set_mode(&mut mode, Mode::Trivial, "--trivial-merge") {
                        return Ok(code);
                    }
                }
                "messages" => show_messages = Some(true),
                "no-messages" => show_messages = Some(false),
                "quiet" => quiet = true,
                "name-only" => name_only = true,
                "allow-unrelated-histories" => allow_unrelated = true,
                "stdin" => use_stdin = true,
                "no-merge-base" => merge_base = None,
                "no-strategy-option" => xopts.clear(),
                "merge-base" => match take_value(args, &mut i, inline) {
                    Some(v) => merge_base = Some(v),
                    None => return Ok(requires_value("option `merge-base'")),
                },
                "strategy-option" => match take_value(args, &mut i, inline) {
                    Some(v) => xopts.push(v),
                    None => return Ok(requires_value("option `strategy-option'")),
                },
                _ => {
                    eprintln!("error: unknown option `{long}'");
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            }
            i += 1;
            continue;
        }

        // A short-option cluster: git's parse-options walks it byte by byte,
        // and `-X` swallows the remainder of the token as its value.
        let cluster = a[1..].to_string();
        let bytes = cluster.as_bytes();
        let mut c = 0;
        while c < bytes.len() {
            match bytes[c] {
                b'z' => {
                    nul = true;
                    c += 1;
                }
                b'X' => {
                    let rest = &cluster[c + 1..];
                    let inline = (!rest.is_empty()).then(|| rest.to_string());
                    match take_value(args, &mut i, inline) {
                        Some(v) => xopts.push(v),
                        None => return Ok(requires_value("switch `X'")),
                    }
                    c = bytes.len();
                }
                // parse_options_step() tests `internal_help` inside the
                // short-option loop: `-h` is answered on stdout at 129, without
                // the `error:` line a rejection carries.
                b'h' => return Ok(super::show_usage(USAGE)),
                other => {
                    eprintln!("error: unknown switch `{}'", other as char);
                    eprint!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
            }
        }
        i += 1;
    }

    // git's first post-parse checks: `--quiet` is mutually exclusive with
    // `--messages`, `--name-only`, `--stdin` and `-z`, in that order (git's
    // four `die_for_incompatible_opt2` calls). Each `die()`s — exit 128 —
    // before it validates the strategy options, the trivial-merge exclusivity
    // rule or the operand count, so they outrank all of those. Parse-time
    // diagnostics (unknown option, `--write-tree`/`--trivial-merge` clash)
    // still win because they fire during parsing, before this point.
    if quiet {
        if show_messages == Some(true) {
            eprintln!("fatal: options '--quiet' and '--messages' cannot be used together");
            return Ok(ExitCode::from(128));
        }
        if name_only {
            eprintln!("fatal: options '--quiet' and '--name-only' cannot be used together");
            return Ok(ExitCode::from(128));
        }
        if use_stdin {
            eprintln!("fatal: options '--quiet' and '--stdin' cannot be used together");
            return Ok(ExitCode::from(128));
        }
        if nul {
            eprintln!("fatal: options '--quiet' and '-z' cannot be used together");
            return Ok(ExitCode::from(128));
        }
    }

    // How many argv slots parse-options consumed as options. `--trivial-merge`
    // tolerates exactly one — itself — and nothing more.
    let options_consumed = original_argc - revs.len();
    if mode == Mode::Trivial && options_consumed > 1 {
        return Ok(trivial_merge_is_exclusive());
    }

    // git validates the collected strategy options before it even looks at how
    // many revisions it was given.
    let mut strategy = StrategyOptions::default();
    for xopt in &xopts {
        if !strategy.absorb(xopt) {
            eprintln!("fatal: unknown strategy option: -X{xopt}");
            return Ok(ExitCode::from(128));
        }
    }

    // git handles `--stdin` right here — after strategy validation and before
    // the operand-count switch, so it never enforces the two-operand rule and
    // simply ignores any positional revs. It reads merges from stdin one per
    // LF-delimited line (`strbuf_getline_lf`, regardless of `-z`), but forces
    // `line_termination = '\0'` for the *output*: every record separator is a
    // NUL and the message block uses the `-z` shape, so `-z` is a no-op here.
    if use_stdin {
        let repo = crate::setup::discover()?;
        let sep = b'\0';
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let mut line = String::new();
        loop {
            line.clear();
            if input.read_line(&mut line)? == 0 {
                break;
            }
            // `strbuf_getline_lf` strips only the trailing LF. git then splits on
            // single spaces with `STRING_LIST_SPLIT_TRIM` and maxsplit -1: each
            // field is trimmed but empties are kept, so a run of spaces inflates
            // `split.nr` into a malformed line.
            let body = line.strip_suffix('\n').unwrap_or(&line);
            let fields: Vec<&str> = body.split(' ').map(str::trim).collect();
            if fields.len() < 2 {
                eprintln!("fatal: malformed input line: '{body}'.");
                return Ok(ExitCode::from(128));
            }
            // git sets the base whenever field[1] is `--`, then only merges when
            // that leaves exactly `<base> -- <b1> <b2>` (nr==4) or, without a base
            // marker, exactly `<b1> <b2>` (nr==2); anything else is malformed.
            let (base, s1, s2) = if fields[1] == "--" && fields.len() == 4 {
                (Some(fields[0]), fields[2], fields[3])
            } else if fields[1] != "--" && fields.len() == 2 {
                (None, fields[0], fields[1])
            } else {
                eprintln!("fatal: malformed input line: '{body}'.");
                return Ok(ExitCode::from(128));
            };

            let mut outcome =
                match resolve_outcome(&repo, base, s1, s2, allow_unrelated, &strategy)? {
                    Ok(o) => o,
                    // A bad operand is a `die()` in git, aborting the whole batch.
                    Err(code) => return Ok(code),
                };
            let conflicted = outcome.has_unresolved_conflicts(TreatAsUnresolved::git());

            // Per-merge record: `printf("%d%c", result.clean, term)`, then the
            // normal single-merge body, then a closing `putchar(term)`.
            let mut rec: Vec<u8> = vec![if conflicted { b'0' } else { b'1' }, sep];
            // `--stdin` forces NUL framing (git's `line_termination = '\0'`), so
            // the body is always rendered in the `-z` shape regardless of `-z`.
            rec.extend_from_slice(&render_outcome(
                &repo,
                &mut outcome,
                name_only,
                show_messages,
                true,
                conflicted,
                s1,
                s2,
            )?);
            rec.push(sep);
            out.write_all(&rec)?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    match mode {
        Mode::Unknown => match revs.len() {
            2 => mode = Mode::Real,
            3 => {
                if options_consumed > 0 {
                    return Ok(trivial_merge_is_exclusive());
                }
                mode = Mode::Trivial;
            }
            _ => {
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        },
        Mode::Real => {
            if revs.len() != 2 {
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
        Mode::Trivial => {
            if revs.len() != 3 {
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
    }

    let repo = crate::setup::discover()?;

    if mode == Mode::Trivial {
        // git's trivial merge peels each of the three operands to a tree before
        // it walks them: an operand that names no object is a fatal `unknown rev`,
        // and one that names a non-tree is `unable to read tree`. Both fire
        // (exit 128) before the walk.
        let mut trees = Vec::with_capacity(3);
        for spec in &revs {
            match resolve_tree(&repo, spec) {
                TreeResolution::Tree(id) => trees.push(id),
                TreeResolution::UnknownRev => {
                    eprintln!("fatal: unknown rev {spec}");
                    return Ok(ExitCode::from(128));
                }
                TreeResolution::NotATree(oid) => {
                    eprintln!("fatal: unable to read tree ({oid})");
                    return Ok(ExitCode::from(128));
                }
            }
        }
        return trivial_merge(&repo, [trees[0], trees[1], trees[2]]);
    }

    let (spec1, spec2) = (revs[0].as_str(), revs[1].as_str());

    // `--quiet` is not a print switch: it sets merge-ort's `mergeability_only`
    // (`cmd_merge_tree()`, builtin/merge-tree.c), and that flag stops the engine
    // *before* it writes anything.
    //
    // ```c
    //         if (opt->mergeability_only) {
    //                 ...
    //                 goto cleanup;
    //         }
    // ```
    //
    // Measured on stock 2.55.0 over a one-hunk content conflict:
    // `cat-file --batch-all-objects` counts 9 objects before and 9 after
    // `merge-tree --write-tree --quiet`, and 11 after the same merge without
    // `--quiet` — the merged blob and its tree. Running the full merge and
    // discarding the *output* left both of those in the object database, which
    // no probe of stdout can see and every probe of the store can.
    //
    // The engine still has to run — the exit code is its verdict — so it runs
    // against an in-memory object store, the same arrangement
    // [`super::merge::virtual_base_tree`] uses for git's unwritten virtual
    // commits, and nothing is persisted when it is dropped.
    if quiet {
        let mut mem = repo.clone();
        mem.objects.enable_object_memory();
        let mut outcome = match resolve_outcome(
            &mem,
            merge_base.as_deref(),
            spec1,
            spec2,
            allow_unrelated,
            &strategy,
        )? {
            Ok(o) => o,
            Err(code) => return Ok(code),
        };
        return Ok(exit_code(outcome.has_unresolved_conflicts(TreatAsUnresolved::git())));
    }

    let mut outcome = match resolve_outcome(
        &repo,
        merge_base.as_deref(),
        spec1,
        spec2,
        allow_unrelated,
        &strategy,
    )? {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };

    let conflicted = outcome.has_unresolved_conflicts(TreatAsUnresolved::git());

    let buf = render_outcome(
        &repo,
        &mut outcome,
        name_only,
        show_messages,
        nul,
        conflicted,
        spec1,
        spec2,
    )?;
    std::io::stdout().lock().write_all(&buf)?;
    Ok(exit_code(conflicted))
}

/// Peel the operands into a merge outcome, shared by the single-merge and
/// `--stdin` batch paths. Returns `Err(code)` when an operand does not name
/// something mergeable — git's diagnostic is already printed and `code` is the
/// exit status of its `die()`/failure (which, in batch mode, aborts the batch).
///
/// The strategy options are folded in *here*, not by the caller, because
/// [`StrategyOptions::apply`] refuses the ones `gix-merge` has no way to express
/// and that refusal must not outrank a bad operand. git validates `-X` values in
/// `cmd_merge_tree` (`parse_merge_opt`, which [`StrategyOptions::absorb`] already
/// mirrors) but *uses* them only inside `real_merge`, after `get_merge_parent`
/// has reported an unmergeable operand. So `-Xignore-cr-at-eol does-not-exist
/// main` is `merge-tree: does-not-exist - not something we can merge` in git, and
/// applying the options any earlier reported the unsupported option instead.
fn resolve_outcome<'repo>(
    repo: &'repo gix::Repository,
    merge_base: Option<&str>,
    spec1: &str,
    spec2: &str,
    allow_unrelated: bool,
    strategy: &StrategyOptions,
) -> Result<std::result::Result<gix::merge::tree::Outcome<'repo>, ExitCode>> {
    let mut labels = Labels {
        ancestor: None,
        current: Some(BStr::new(spec1)),
        other: Some(BStr::new(spec2)),
    };
    // Only the `-Xsubtree` path needs it, and it has to outlive `labels`.
    let ancestor_name: String;
    // Both branches produce the same tree-merge outcome; only how the ancestor
    // is chosen differs.
    let outcome = if let Some(base_spec) = merge_base {
        // With an explicit base, git accepts plain trees for all three sides,
        // and reports any side that will not peel to one as a fatal error.
        // Sequentially, and stopping at the first that will not peel — git runs
        // the three `repo_get_oid_treeish()` calls one after another and `die()`s
        // inside the first failing `if`, so a later operand is never resolved at
        // all. Resolving all three and *then* re-scanning for the bad one would
        // both reach past the die and put a second `get_oid_basic()` on every
        // operand, which is one ambiguity warning too many for each.
        let mut peeled = Vec::with_capacity(3);
        for spec in [base_spec, spec1, spec2] {
            let Some(id) = peel_tree(repo, spec) else {
                eprintln!("fatal: could not parse as tree '{spec}'");
                return Ok(Err(ExitCode::from(128)));
            };
            peeled.push(id);
        }
        let (base, ours, theirs) = (peeled[0], peeled[1], peeled[2]);
        // `init_merge_options()` runs after the operands are peeled and can die
        // on a bad `merge.renameLimit` — see
        // [`super::merge::merge_recursive_config_check`].
        if let Some(code) = super::merge::merge_recursive_config_check(repo) {
            return Ok(Err(code));
        }
        let (base, theirs) = strategy.shift(repo, ours, base, theirs)?;
        repo.merge_trees(base, ours, theirs, labels, strategy.apply(repo.tree_merge_options()?)?)?
    } else {
        let Some(ours) = peel_commit(repo, spec1) else {
            eprintln!("merge-tree: {spec1} - not something we can merge");
            return Ok(Err(ExitCode::FAILURE));
        };
        let Some(theirs) = peel_commit(repo, spec2) else {
            eprintln!("merge-tree: {spec2} - not something we can merge");
            return Ok(Err(ExitCode::FAILURE));
        };
        // `init_merge_options()` runs once both operands have peeled, and its
        // `merge_recursive_config()` can die — see
        // [`super::merge::merge_recursive_config_check`].
        if let Some(code) = super::merge::merge_recursive_config_check(repo) {
            return Ok(Err(code));
        }
        let bases = repo.merge_bases_many(ours, &[theirs])?;
        if !allow_unrelated && bases.is_empty() {
            eprintln!("fatal: refusing to merge unrelated histories");
            return Ok(Err(ExitCode::from(128)));
        }
        // The base is materialized here rather than left to `merge_commits()`, for two
        // reasons: `-s subtree` shifts inside `merge_ort_nonrecursive_internal()`, i.e. once
        // the merge base is already settled, so `merge_commits()` would merge the unshifted
        // trees; and git's virtual merge commits are allocated, never written
        // (`make_virtual_commit()`, merge-ort.c), which `merge_commits()` cannot express.
        // It is chosen exactly as `gix_merge::commit()` chooses it, including the ancestor
        // label the diff3 styles print.
        let options = strategy.apply(repo.tree_merge_options()?)?;
        let base = match bases.len() {
            0 => {
                ancestor_name = "empty tree".into();
                ObjectId::empty_tree(repo.object_hash())
            }
            1 => {
                ancestor_name = bases[0].shorten_or_id().to_string();
                repo.find_commit(bases[0])?.tree_id()?.detach()
            }
            _ => {
                ancestor_name = "merged common ancestors".into();
                let bases: Vec<ObjectId> = bases.iter().map(|id| id.detach()).collect();
                super::merge::virtual_base_tree(repo, &bases)?
            }
        };
        labels.ancestor = Some(BStr::new(ancestor_name.as_bytes()));
        let ours_tree = repo.find_commit(ours)?.tree_id()?.detach();
        let theirs_tree = repo.find_commit(theirs)?.tree_id()?.detach();
        let (base, theirs_tree) = strategy.shift(repo, ours_tree, base, theirs_tree)?;
        repo.merge_trees(base, ours_tree, theirs_tree, labels, options)?
    };
    Ok(Ok(outcome))
}

/// Render one resolved merge to git's single-merge byte layout: the toplevel
/// tree id, then (when conflicted) the per-stage `<mode> <object> <stage>\t<path>`
/// lines (or bare paths under `--name-only`), then the message block. This is
/// exactly what `real_merge` prints for a non-`--stdin` merge; the batch path
/// wraps it with the clean flag and trailing separator.
#[allow(clippy::too_many_arguments)]
fn render_outcome(
    repo: &gix::Repository,
    outcome: &mut gix::merge::tree::Outcome<'_>,
    name_only: bool,
    show_messages: Option<bool>,
    nul: bool,
    conflicted: bool,
    label1: &str,
    label2: &str,
) -> Result<Vec<u8>> {
    let how = TreatAsUnresolved::git();
    // Render everything up front so an unrenderable conflict class fails before
    // a single byte reaches stdout.
    let mut buf: Vec<u8> = Vec::new();
    let sep = if nul { b'\0' } else { b'\n' };

    let tree_id = outcome.tree.write()?.detach();
    buf.extend_from_slice(tree_id.to_string().as_bytes());
    buf.push(sep);

    if conflicted {
        let mut index = repo.index_from_tree(&tree_id)?;
        outcome.index_changed_after_applying_conflicts(&mut index, how, RemovalMode::Prune);
        let mut last_path: Option<BString> = None;
        for entry in index.entries() {
            let stage = entry.stage_raw();
            if stage == 0 {
                continue;
            }
            let path = entry.path(&index);
            if name_only {
                // One line per path, however many stages it has.
                if last_path.as_ref().map(|p| p.as_bstr()) == Some(path) {
                    continue;
                }
                last_path = Some(path.to_owned());
                buf.extend_from_slice(&render_path(path, nul));
            } else {
                let line = format!("{:06o} {} {stage}\t", entry.mode.bits(), entry.id.to_hex());
                buf.extend_from_slice(line.as_bytes());
                buf.extend_from_slice(&render_path(path, nul));
            }
            buf.push(sep);
        }
    }

    if show_messages.unwrap_or(conflicted) {
        // `merge-tree`'s stdout is a machine-readable record and nothing has been
        // written when this runs, so an unrenderable conflict class is refused
        // rather than approximated — the strict half of [`crate::merge_msg`].
        let messages = crate::merge_msg::render(
            repo,
            &outcome.conflicts,
            label1,
            label2,
            crate::merge_msg::Operand1::Spec(label1),
            how,
            crate::merge_msg::Strictness::Refuse,
        )?;
        if nul {
            // The `-z` messages section opens with its own NUL separator, then
            // carries one `<count>\0<path>\0...\0<type>\0<message>\0` record per
            // entry, mirroring git's `merge_display_update_messages(detailed=1)`:
            // it prints `info->paths.nr`, every path, the short type, then the
            // message (whose own trailing newline is retained, since git emits it
            // with `puts()` before the record-closing NUL).
            buf.push(b'\0');
            for m in &messages {
                buf.extend_from_slice(m.paths.len().to_string().as_bytes());
                buf.push(b'\0');
                for path in &m.paths {
                    buf.extend_from_slice(path.as_slice());
                    buf.push(b'\0');
                }
                buf.extend_from_slice(m.ctype.as_bytes());
                buf.push(b'\0');
                buf.extend_from_slice(m.text.as_bytes());
                buf.push(b'\0');
            }
        } else {
            buf.push(b'\n');
            for m in &messages {
                buf.extend_from_slice(m.text.as_bytes());
            }
        }
    }

    Ok(buf)
}

/// Select `wanted` as the command mode, reporting git's parse-options clash
/// diagnostic when a different mode was already chosen.
///
/// git names the option it is currently looking at first, then the one already
/// in effect, and exits 129 without printing the usage block.
fn set_mode(mode: &mut Mode, wanted: Mode, flag: &str) -> Option<ExitCode> {
    let existing = match *mode {
        Mode::Unknown => {
            *mode = wanted;
            return None;
        }
        Mode::Real => "--write-tree",
        Mode::Trivial => "--trivial-merge",
    };
    if *mode == wanted {
        return None;
    }
    eprintln!("error: options '{flag}' and '{existing}' cannot be used together");
    Some(ExitCode::from(129))
}

/// Take an option's value: the `=`-attached one when present, otherwise the
/// next argument, advancing `i` onto it. `None` when the value is missing.
fn take_value(args: &[String], i: &mut usize, inline: Option<String>) -> Option<String> {
    if let Some(v) = inline {
        return Some(v);
    }
    let v = args.get(*i + 1)?.clone();
    *i += 1;
    Some(v)
}

/// git's bare "requires a value" diagnostic — no usage block, exit 129.
fn requires_value(what: &str) -> ExitCode {
    eprintln!("error: {what} requires a value");
    ExitCode::from(129)
}

/// git's refusal to combine `--trivial-merge` with anything else.
fn trivial_merge_is_exclusive() -> ExitCode {
    eprintln!("fatal: --trivial-merge is incompatible with all other options");
    ExitCode::from(128)
}

impl StrategyOptions {
    /// Absorb one `-X` value, returning `false` for anything git's
    /// `parse_merge_opt()` rejects. Later values win, exactly as in git.
    pub(super) fn absorb(&mut self, s: &str) -> bool {
        match s {
            "ours" => self.favor = Some(FileFavor::Ours),
            "theirs" => self.favor = Some(FileFavor::Theirs),
            "subtree" => self.subtree = Some(String::new()),
            "patience" => self.diff_algorithm = Some("patience".into()),
            "histogram" => self.diff_algorithm = Some("histogram".into()),
            "ignore-space-change" => self.whitespace |= XDF_IGNORE_WHITESPACE_CHANGE,
            "ignore-all-space" => self.whitespace |= XDF_IGNORE_WHITESPACE,
            "ignore-space-at-eol" => self.whitespace |= XDF_IGNORE_WHITESPACE_AT_EOL,
            "ignore-cr-at-eol" => self.whitespace |= XDF_IGNORE_CR_AT_EOL,
            "renormalize" => self.renormalize = Some(true),
            "no-renormalize" => self.renormalize = Some(false),
            "no-renames" => self.detect_renames = Some(false),
            "find-renames" => {
                self.detect_renames = Some(true);
                self.rename_score = Some(0);
            }
            _ => {
                if let Some(path) = s.strip_prefix("subtree=") {
                    self.subtree = Some(path.to_string());
                } else if let Some(name) = s.strip_prefix("diff-algorithm=") {
                    let name = name.to_ascii_lowercase();
                    if !matches!(
                        name.as_str(),
                        "myers" | "default" | "minimal" | "patience" | "histogram"
                    ) {
                        return false;
                    }
                    self.diff_algorithm = Some(name);
                } else if let Some(score) = s
                    .strip_prefix("find-renames=")
                    .or_else(|| s.strip_prefix("rename-threshold="))
                {
                    let Some(score) = parse_rename_score(score) else {
                        return false;
                    };
                    self.rename_score = Some(score);
                    self.detect_renames = Some(true);
                } else {
                    return false;
                }
            }
        }
        true
    }

    /// Fold the strategy options into the merge options, refusing the ones
    /// `gix-merge` has no way to express rather than silently ignoring them.
    pub(super) fn apply(
        &self,
        options: gix::merge::tree::Options,
    ) -> Result<gix::merge::tree::Options> {
        if self.renormalize == Some(true) {
            anyhow::bail!("unsupported strategy option \"renormalize\" (gix-merge's blob pipeline is not driven in renormalizing mode here)");
        }

        let algorithm = match self.diff_algorithm.as_deref() {
            // `init_merge_options()` (merge-ort.c) opens with
            // `opt->xdl_opts = DIFF_WITH_ALG(opt, HISTOGRAM_DIFF)`, so an
            // un-asked-for merge is a *histogram* merge, not a Myers one.
            // `merge-tree` takes the `init_basic_merge_options()` door, which is
            // the same but for skipping the `diff.algorithm` config read — so no
            // configuration can move this, only `-X`.
            None => Some(gix::diff::blob::Algorithm::Histogram),
            Some("myers" | "default") => Some(gix::diff::blob::Algorithm::Myers),
            Some("minimal") => Some(gix::diff::blob::Algorithm::MyersMinimal),
            Some("histogram") => Some(gix::diff::blob::Algorithm::Histogram),
            Some("patience") => Some(gix::diff::blob::Algorithm::Patience),
            Some(other) => anyhow::bail!(
                "unsupported strategy option \"{other}\" diff algorithm (gix-imara-diff implements myers, minimal, patience and histogram only)"
            ),
        };

        // The rewrite and blob-merge knobs only exist on the plumbing options,
        // so round-trip through them before applying the builder-level ones.
        let mut plumbing: gix::merge::plumbing::tree::Options = options.into();
        if let Some(algorithm) = algorithm {
            plumbing.blob_merge.text.diff_algorithm = algorithm;
        }
        plumbing.blob_merge.text.canonicalize = self.canonicalize();
        if self.detect_renames == Some(false) {
            plumbing.rewrites = None;
        } else if let Some(score) = self.rename_score {
            let mut rewrites = plumbing.rewrites.unwrap_or_default();
            // A score of zero is git's "just use the default threshold".
            if score > 0 {
                rewrites.percentage = Some(score as f32 / MAX_SCORE as f32);
            }
            plumbing.rewrites = Some(rewrites);
        }

        let options = gix::merge::tree::Options::from(plumbing);
        Ok(match self.favor {
            Some(favor) => options.with_file_favor(Some(favor)),
            None => options,
        })
    }

    /// `xpp.flags`' whitespace rule as the canonical form `xdl_recmatch()` groups
    /// records by, or `None` when no `-Xignore-*` was given.
    ///
    /// The order of the tests is `xdl_recmatch()`'s own `else if` chain, which is
    /// what makes the flags a hierarchy rather than a set: `-w` matches everything
    /// `-b` matches, `-b` everything `--ignore-space-at-eol` matches, and that
    /// everything `--ignore-cr-at-eol` matches.
    ///
    /// The rules themselves are [`crate::merge_ws::canonicalize_for`], the one
    /// place the merge family ports `xdl_recmatch()` — `git merge -X` reaches the
    /// same function with the same `xdl_opts` word.
    fn canonicalize(&self) -> Option<gix::merge::blob::builtin_driver::text::Canonicalize> {
        crate::merge_ws::canonicalize_for(self.whitespace)
    }

    /// `merge_ort_nonrecursive_internal()`'s opening move: with `-Xsubtree` set,
    /// *their* tree and the merge base are shifted to match the shape of *our*
    /// tree before any merge information is collected, so the merged tree comes
    /// out in our shape.
    ///
    /// The shifting itself is `match-trees.c`, already ported for `git merge` in
    /// [`crate::merge_apply::shift_tree_object`]; merge-tree drives its own merge
    /// but shifts from the same one place git does.
    fn shift(
        &self,
        repo: &gix::Repository,
        ours: ObjectId,
        base: ObjectId,
        theirs: ObjectId,
    ) -> Result<(ObjectId, ObjectId)> {
        let Some(prefix) = self.subtree.as_deref() else {
            return Ok((base, theirs));
        };
        let prefix = BStr::new(prefix.as_bytes());
        Ok((
            crate::merge_apply::shift_tree_object(repo, ours, base, prefix)?,
            crate::merge_apply::shift_tree_object(repo, ours, theirs, prefix)?,
        ))
    }
}

/// Port of git's `parse_rename_score()`: a decimal number, optionally
/// fractional and optionally `%`-suffixed, scaled onto [`MAX_SCORE`].
///
/// `None` when anything is left over after the number, which is how git
/// distinguishes `-Xfind-renames=50` from `-Xfind-renames=abc`.
fn parse_rename_score(s: &str) -> Option<u32> {
    let bytes = s.as_bytes();
    let (mut num, mut scale, mut dot) = (0u64, 1u64, false);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'.' if !dot => {
                scale = 1;
                dot = true;
            }
            b'%' => {
                scale = scale.saturating_mul(100);
                i += 1;
                break;
            }
            c if c.is_ascii_digit() => {
                num = num.saturating_mul(10).saturating_add(u64::from(c - b'0'));
                if dot {
                    scale = scale.saturating_mul(10);
                }
            }
            _ => break,
        }
        i += 1;
    }
    if i != bytes.len() {
        return None;
    }
    Some(if num >= scale {
        MAX_SCORE as u32
    } else {
        (MAX_SCORE.saturating_mul(num) / scale) as u32
    })
}

/// `1` when the merge had unresolved conflicts, `0` otherwise — git's contract.
fn exit_code(conflicted: bool) -> ExitCode {
    if conflicted {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Resolve `spec` to the tree it names (commits and tags peel through), or
/// `None` when git would say it could not parse it as a tree.
fn peel_tree(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    // One `repo_get_oid_treeish()` per operand, so one trip through
    // `get_oid_basic()` and one chance at everything it raises — the 40-hex
    // ambiguity warning, `read_ref_at()`'s two, `interpret_branch_mark()`'s
    // `die()` and `peel_onion()`'s `error()`. Reaching only the first left stock's
    // `warning: log for 'HEAD' only goes back to …` unsaid for
    // `git merge-tree 'HEAD@{<old date>}' HEAD`.
    let id = crate::objname::resolve(repo, spec)?;
    Some(repo.find_object(id).ok()?.peel_to_tree().ok()?.id)
}

/// Resolve `spec` to a commit id, or `None` when it is not something git would
/// accept as a side of the merge.
///
/// `get_merge_parent()` (builtin/merge-tree.c) peels through `peel_to_type()`
/// (object.c), which distinguishes the two ways this fails: a spec that names no
/// object at all is silent — the caller's `not something we can merge` is the
/// whole diagnostic — while a spec that *does* resolve, but to an object that
/// only dereferences to a tree or a blob, is reported first, by name and by the
/// type it landed on. Tags are followed on the way, so the type named is the one
/// at the end of the tag chain rather than `tag`.
fn peel_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    // `get_merge_parent()` is one `repo_get_oid()` (`commit.c:1881`) ahead of the
    // peel, which is where every one of `get_oid_basic()`'s diagnostics comes
    // from — not the ambiguity warning alone.
    let id = crate::objname::resolve(repo, spec)?;
    let object = repo.find_object(id).ok()?;
    let object = object.peel_tags_to_end().ok()?;
    if object.kind == gix::object::Kind::Commit {
        return Some(object.id);
    }
    eprintln!(
        "error: {spec}: expected commit type, but the object dereferences to {} type",
        object.kind
    );
    None
}

/// Outcome of resolving a trivial-merge operand, mirroring the two distinct
/// failure modes of git's `get_tree_descriptor()`.
enum TreeResolution {
    /// The spec peels to this tree.
    Tree(ObjectId),
    /// The spec names no object at all — git's `unknown rev`.
    UnknownRev,
    /// The spec resolves but does not peel to a tree — git's `unable to read
    /// tree`, which names the resolved object id.
    NotATree(ObjectId),
}

/// Resolve `spec` the way git's trivial merge does: to a tree, or one of the two
/// fatal conditions it distinguishes before it begins the three-tree walk.
fn resolve_tree(repo: &gix::Repository, spec: &str) -> TreeResolution {
    // `get_tree_descriptor()`'s `repo_get_oid(r, rev, &oid)`
    // (`builtin/merge-tree.c:379`), one per operand — and therefore one trip
    // through `get_oid_basic()`, which says more than the ambiguity warning this
    // used to be.
    let id = match crate::objname::resolve(repo, spec) {
        Some(id) => id,
        None => return TreeResolution::UnknownRev,
    };
    let object = match repo.find_object(id) {
        Ok(object) => object,
        Err(_) => return TreeResolution::UnknownRev,
    };
    let oid = object.id;
    match object.peel_to_tree() {
        Ok(tree) => TreeResolution::Tree(tree.id),
        Err(_) => TreeResolution::NotATree(oid),
    }
}

// ---------------------------------------------------------------------------
// `--trivial-merge`: the original 2005 three-tree walk
// ---------------------------------------------------------------------------

/// One `struct merge_list` node: one stage of one path. A path's stages are held
/// in one `Vec` in `link` order, which is the order `show_result_list()` prints.
struct MergeEntry {
    /// 0 result, 1 base, 2 our, 3 their — indexes `desc[]`.
    stage: u8,
    mode: u32,
    id: ObjectId,
    path: BString,
}

/// One tree entry at the current level, or (with `mode == 0` and a null id) the
/// absence of one — git's `struct name_entry` for a tree that lacks the name.
#[derive(Clone)]
struct NameEntry {
    name: BString,
    mode: u32,
    id: ObjectId,
}

impl NameEntry {
    fn is_dir(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
    fn is_null(&self) -> bool {
        self.id.is_null()
    }
}

/// `trivial_merge()`: walk `t[0]` (base), `t[1]` (ours) and `t[2]` (theirs) in
/// lock-step and print what the three-way comparison of every name found.
///
/// Not covered: `xpp.flags`-driven whitespace options (the deprecated mode takes
/// none) and git's binary merge driver's `LL_MERGE_BINARY_CONFLICT` return, which
/// this mode never reports — a binary conflict still yields our side's content,
/// as `ll_binary_merge()` does.
fn trivial_merge(repo: &gix::Repository, trees: [ObjectId; 3]) -> Result<ExitCode> {
    let mut result: Vec<Vec<MergeEntry>> = Vec::new();
    let descs = [
        read_tree(repo, Some(trees[0]))?,
        read_tree(repo, Some(trees[1]))?,
        read_tree(repo, Some(trees[2]))?,
    ];
    trivial_merge_trees(repo, descs, b"".as_bstr(), &mut result)?;

    // `show_result()`.
    let mut out = std::io::stdout().lock();
    for item in &result {
        show_result_list(&mut out, item)?;
        show_diff(repo, &mut out, item)?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// `fill_tree_descriptor()`: a tree's entries in tree order, or nothing at all
/// for the absent side of a directory that only some operands have.
fn read_tree(repo: &gix::Repository, id: Option<ObjectId>) -> Result<Vec<NameEntry>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let tree = repo.find_tree(id)?;
    let mut out = Vec::new();
    for entry in tree.iter() {
        let entry = entry?;
        out.push(NameEntry {
            name: entry.filename().to_owned(),
            mode: entry.mode().value() as u32,
            id: entry.oid().to_owned(),
        });
    }
    Ok(out)
}

/// `traverse_trees()` restricted to what `trivial_merge_trees()` needs: advance
/// the three cursors together, grouping the entries `df_name_compare()` calls
/// equal — which is why a file and a directory of the same name arrive together
/// and reach `unresolved_directory()`.
fn trivial_merge_trees(
    repo: &gix::Repository,
    trees: [Vec<NameEntry>; 3],
    base: &BStr,
    out: &mut Vec<Vec<MergeEntry>>,
) -> Result<()> {
    let mut idx = [0usize; 3];
    loop {
        let mut min: Option<NameEntry> = None;
        for (i, tree) in trees.iter().enumerate() {
            if let Some(e) = tree.get(idx[i]) {
                if min
                    .as_ref()
                    .map_or(true, |m| df_name_compare(e, m) == std::cmp::Ordering::Less)
                {
                    min = Some(e.clone());
                }
            }
        }
        let Some(min) = min else { return Ok(()) };

        let null = ObjectId::null(repo.object_hash());
        let mut n: [NameEntry; 3] = std::array::from_fn(|_| NameEntry {
            name: min.name.clone(),
            mode: 0,
            id: null,
        });
        for (i, tree) in trees.iter().enumerate() {
            if let Some(e) = tree.get(idx[i]) {
                if df_name_compare(e, &min) == std::cmp::Ordering::Equal {
                    n[i] = e.clone();
                    idx[i] += 1;
                }
            }
        }
        threeway_callback(repo, base, &n, out)?;
    }
}

/// `df_name_compare()`: like `base_name_compare()`, except that a directory and
/// a file of the same name compare *equal*, so the walk hands both to the same
/// callback.
fn df_name_compare(a: &NameEntry, b: &NameEntry) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (n1, n2) = (a.name.as_slice(), b.name.as_slice());
    let len = n1.len().min(n2.len());
    match n1[..len].cmp(&n2[..len]) {
        Ordering::Equal => {}
        other => return other,
    }
    if n1.len() == n2.len() {
        return Ordering::Equal;
    }
    // C reads the NUL terminator for the shorter name, and substitutes `/` for
    // it when that side is a directory.
    let at = |n: &[u8], e: &NameEntry| -> u8 {
        match n.get(len) {
            Some(&c) => c,
            None if e.is_dir() => b'/',
            None => 0,
        }
    };
    let (c1, c2) = (at(n1, a), at(n2, b));
    if (c1 == b'/' && c2 == 0) || (c2 == b'/' && c1 == 0) {
        return Ordering::Equal;
    }
    c1.cmp(&c2)
}

/// `same_entry()`: "An empty entry never compares same, not even to another
/// empty entry".
fn same_entry(a: &NameEntry, b: &NameEntry) -> bool {
    !a.is_null() && !b.is_null() && a.id == b.id && a.mode == b.mode
}

fn both_empty(a: &NameEntry, b: &NameEntry) -> bool {
    a.is_null() && b.is_null()
}

/// `threeway_callback()`: the read-tree three-way resolution rules.
fn threeway_callback(
    repo: &gix::Repository,
    base: &BStr,
    n: &[NameEntry; 3],
    out: &mut Vec<Vec<MergeEntry>>,
) -> Result<()> {
    // Modified, added or removed identically.
    if same_entry(&n[1], &n[2]) || both_empty(&n[1], &n[2]) {
        return Ok(()); // `resolve(info, NULL, …)` shows nothing.
    }

    if same_entry(&n[0], &n[1]) && !n[2].is_null() && !n[2].is_dir() {
        // We did not touch, they modified — take theirs.
        resolve(base, &n[1], &n[2], out);
        return Ok(());
    }
    // Otherwise (a directory on one side, a file on the other) fall through to
    // `unresolved()`, which recurses.

    // We added, modified or removed, they did not touch — take ours.
    if same_entry(&n[0], &n[2]) || both_empty(&n[0], &n[2]) {
        return Ok(()); // again `resolve(info, NULL, …)`.
    }

    unresolved(repo, base, n, out)
}

/// `resolve()` with a non-NULL `ours`: the merged result plus the version it
/// replaced, so `show_diff()` can show what taking theirs changed.
fn resolve(base: &BStr, ours: &NameEntry, result: &NameEntry, out: &mut Vec<Vec<MergeEntry>>) {
    let path = traverse_path(base, result.name.as_bstr());
    out.push(vec![
        MergeEntry { stage: 0, mode: result.mode, id: result.id, path: path.clone() },
        MergeEntry { stage: 2, mode: ours.mode, id: ours.id, path },
    ]);
}

/// `unresolved()`: recurse into any directory at this name first, then record
/// the non-directory stages that are left.
fn unresolved(
    repo: &gix::Repository,
    base: &BStr,
    n: &[NameEntry; 3],
    out: &mut Vec<Vec<MergeEntry>>,
) -> Result<()> {
    // A missing entry counts as a directory, so a name that is a directory
    // wherever it exists is fully handled by the recursion below.
    let mask = 0b111u8;
    let mut dirmask = 0u8;
    for (i, e) in n.iter().enumerate() {
        if e.mode == 0 || e.is_dir() {
            dirmask |= 1 << i;
        }
    }

    unresolved_directory(repo, base, n, out)?;

    if dirmask == mask {
        return Ok(());
    }

    // `link_entry()` builds the chain back to front, so `their` ends up last.
    let mut chain: Vec<MergeEntry> = Vec::new();
    let mut path: Option<BString> = None;
    for (stage, i) in [(3u8, 2usize), (2, 1), (1, 0)] {
        let e = &n[i];
        if e.mode == 0 || e.is_dir() {
            continue;
        }
        // Every stage of one path shares the first path string that was built.
        let p = path.get_or_insert_with(|| traverse_path(base, e.name.as_bstr())).clone();
        chain.insert(0, MergeEntry { stage, mode: e.mode, id: e.id, path: p });
    }
    if !chain.is_empty() {
        out.push(chain);
    }
    Ok(())
}

/// `unresolved_directory()`: descend into the sub-trees of whichever operands
/// have a directory here, treating the others as absent.
fn unresolved_directory(
    repo: &gix::Repository,
    base: &BStr,
    n: &[NameEntry; 3],
    out: &mut Vec<Vec<MergeEntry>>,
) -> Result<()> {
    let Some(first_dir) = n.iter().find(|e| e.mode != 0 && e.is_dir()) else {
        return Ok(()); /* there is no tree here */
    };
    let newbase = traverse_path(base, first_dir.name.as_bstr());
    let sub = [
        read_tree(repo, dir_id(&n[0]))?,
        read_tree(repo, dir_id(&n[1]))?,
        read_tree(repo, dir_id(&n[2]))?,
    ];
    trivial_merge_trees(repo, sub, newbase.as_bstr(), out)
}

/// `ENTRY_OID()`: the entry's id only when it really is a directory.
fn dir_id(e: &NameEntry) -> Option<ObjectId> {
    (e.mode != 0 && e.is_dir()).then_some(e.id)
}

/// `strbuf_make_traverse_path()`: the base and the entry name joined by `/`.
fn traverse_path(base: &BStr, name: &BStr) -> BString {
    if base.is_empty() {
        return name.to_owned();
    }
    let mut out = base.to_owned();
    out.push(b'/');
    out.extend_from_slice(name);
    out
}

/// `explanation()`: the headline above one path's stages.
fn explanation(item: &[MergeEntry]) -> &'static str {
    match item[0].stage {
        0 => "merged",
        3 => "added in remote",
        2 => {
            if item.len() > 1 {
                "added in both"
            } else {
                "added in local"
            }
        }
        // Existed in base.
        _ => match item.get(1) {
            None => "removed in both",
            Some(second) => {
                if item.len() > 2 {
                    "changed in both"
                } else if second.stage == 3 {
                    "removed in local"
                } else {
                    "removed in remote"
                }
            }
        },
    }
}

/// `show_result_list()`: the headline, then one `  <desc> <mode> <oid> <path>`
/// line per stage.
fn show_result_list(out: &mut impl Write, item: &[MergeEntry]) -> Result<()> {
    const DESC: [&str; 4] = ["result", "base", "our", "their"];
    write!(out, "{}\n", explanation(item))?;
    for e in item {
        write!(
            out,
            "  {:<6} {:o} {} {}\n",
            DESC[e.stage as usize], e.mode, e.id, e.path
        )?;
    }
    Ok(())
}

/// `show_diff()`: a bare unified diff (context 3, no file headers) from our
/// version of the path to the merged result.
fn show_diff(repo: &gix::Repository, out: &mut impl Write, item: &[MergeEntry]) -> Result<()> {
    let src = origin(repo, item).unwrap_or_default();
    let dst = merge_result(repo, item).unwrap_or_default();

    let before = super::diff::byte_lines(&src);
    let after = super::diff::byte_lines(&dst);
    let mut input: InternedInput<Vec<u8>> = InternedInput::default();
    input.update_before(before.iter().map(|l| l.to_vec()));
    input.update_after(after.iter().map(|l| l.to_vec()));
    let diff = diff_with_slider_heuristics(gix::diff::blob::Algorithm::Myers, &input);
    if diff.count_additions() == 0 && diff.count_removals() == 0 {
        return Ok(());
    }
    let hunks = UnifiedDiff::new(
        &diff,
        &input,
        TrivialSink { buf: Vec::new(), before: &before, after: &after },
        ContextSize::symmetrical(3),
    )
    .consume()?;
    out.write_all(&hunks)?;
    Ok(())
}

/// `origin()`: our version of the path, or nothing when we do not have one.
fn origin(repo: &gix::Repository, item: &[MergeEntry]) -> Option<Vec<u8>> {
    let e = item.iter().find(|e| e.stage == 2)?;
    read_blob(repo, e.id)
}

/// `result()`: a stage-0 entry is the merged blob itself; anything else is fed
/// through `merge_blobs()`.
fn merge_result(repo: &gix::Repository, item: &[MergeEntry]) -> Option<Vec<u8>> {
    if item[0].stage == 0 {
        return read_blob(repo, item[0].id);
    }
    let stage = |s: u8| item.iter().find(|e| e.stage == s);
    merge_blobs(
        repo,
        stage(1).map(|e| e.id),
        stage(2).map(|e| e.id),
        stage(3).map(|e| e.id),
    )
}

/// `merge_blobs()` (merge-blobs.c): a side missing on either branch resolves to
/// whichever side still has content — unless the path existed in the base, in
/// which case the merge produces nothing at all. Two present sides go through
/// `ll_merge()` with the `.our`/`.their` labels and no ancestor label.
///
/// ```c
/// if (git_xmerge_style >= 0)
///         xmp.style = git_xmerge_style;
/// ```
///
/// (`ll_xdl_merge()`, ll-merge.c.) `git_xmerge_style` is `merge.conflictStyle`, read by
/// `git_xmerge_config()` for every command that merges content — the deprecated
/// three-tree mode included, which is why a `diff3` conflict here carries the `|||||||`
/// section even though the mode names no ancestor label to put after it.
fn merge_blobs(
    repo: &gix::Repository,
    base: Option<ObjectId>,
    ours: Option<ObjectId>,
    theirs: Option<ObjectId>,
) -> Option<Vec<u8>> {
    let (Some(our_id), Some(their_id)) = (ours, theirs) else {
        if base.is_some() {
            return None;
        }
        return read_blob(repo, ours.or(theirs)?);
    };
    let our = read_blob(repo, our_id)?;
    let their = read_blob(repo, their_id)?;
    let ancestor = base.and_then(|id| read_blob(repo, id)).unwrap_or_default();

    // `ll_merge()` hands a binary buffer to `ll_binary_merge()`, whose default
    // variant keeps our side verbatim and reports a conflict.
    if buffer_is_binary(&our) || buffer_is_binary(&their) || buffer_is_binary(&ancestor) {
        return Some(our);
    }

    let style = super::merge_file::conflict_style_config(Some(repo)).unwrap_or(ConflictStyle::Merge);
    let mut merged = Vec::new();
    let mut input = InternedInput::default();
    // `Merge::new` takes the operands in `git merge-file` order —
    // current, ancestor, other — not `ll_merge`'s ancestor-first order.
    let merge = TextMerge::new(
        &mut input,
        &our,
        &ancestor,
        &their,
        gix::diff::blob::Algorithm::Myers,
    );
    merge.run_with(
        &mut merged,
        Labels {
            ancestor: None,
            current: Some(b".our".as_bstr()),
            other: Some(b".their".as_bstr()),
        },
        Rendering {
            conflict: MergeConflict::Keep {
                style,
                marker_size: std::num::NonZeroU8::new(7).expect("nonzero"),
            },
            style: Some(style),
            // `ll_xdl_merge()` sets `xmp.level = XDL_MERGE_ZEALOUS`.
            level: Level::Zealous,
            marker_size: Some(7),
        },
    );
    Some(merged)
}

/// `buffer_is_binary()`: a NUL in the first 8000 bytes.
fn buffer_is_binary(data: &[u8]) -> bool {
    data[..data.len().min(8000)].contains(&0)
}

/// `odb_read_object()`: the object's bytes, or nothing when it is not there.
fn read_blob(repo: &gix::Repository, id: ObjectId) -> Option<Vec<u8>> {
    repo.find_object(id).ok().map(|o| o.data.clone())
}

/// The `xdi_diff` sink for [`show_diff`]: hunk headers and body lines only, with
/// no `---`/`+++` header and no function-context suffix (the deprecated mode
/// leaves `xecfg.flags` at zero).
struct TrivialSink<'a> {
    buf: Vec<u8>,
    before: &'a [&'a [u8]],
    after: &'a [&'a [u8]],
}

impl ConsumeHunk for TrivialSink<'_> {
    type Out = Vec<u8>;

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        let range = |start: u32, len: u32| match len {
            1 => format!("{start}"),
            0 => format!("{},0", start.saturating_sub(1)),
            _ => format!("{start},{len}"),
        };
        self.buf.extend_from_slice(
            format!(
                "@@ -{} +{} @@\n",
                range(header.before_hunk_start, header.before_hunk_len),
                range(header.after_hunk_start, header.after_hunk_len)
            )
            .as_bytes(),
        );

        let mut bi = header.before_hunk_start.saturating_sub(1) as usize;
        let mut ai = header.after_hunk_start.saturating_sub(1) as usize;
        for (kind, fallback) in lines {
            let (marker, content): (u8, &[u8]) = match kind {
                DiffLineKind::Context => {
                    let c = self.after.get(ai).copied().unwrap_or(fallback);
                    bi += 1;
                    ai += 1;
                    (b' ', c)
                }
                DiffLineKind::Remove => {
                    let c = self.before.get(bi).copied().unwrap_or(fallback);
                    bi += 1;
                    (b'-', c)
                }
                DiffLineKind::Add => {
                    let c = self.after.get(ai).copied().unwrap_or(fallback);
                    ai += 1;
                    (b'+', c)
                }
            };
            self.buf.push(marker);
            self.buf.extend_from_slice(content);
            if content.last() != Some(&b'\n') {
                self.buf.push(b'\n');
                self.buf
                    .extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// A path as it appears in the conflicted-file-info section: raw under `-z`,
/// otherwise C-quoted by `quote_c_style()`.
fn render_path(path: &BStr, nul: bool) -> Vec<u8> {
    if nul {
        path.to_vec()
    } else {
        quote_path(path).into_bytes()
    }
}

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    crate::quote::quoted_name_string(path.as_ref())
}
