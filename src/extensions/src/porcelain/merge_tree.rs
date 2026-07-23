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
//!     `diff-algorithm=myers|default|minimal|histogram`, and the no-op
//!     `no-renormalize`
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
//! Not covered, and refused rather than approximated:
//!   * actually *running* the deprecated `--trivial-merge` mode (its
//!     option-compatibility rules are enforced and its operands are peeled to
//!     trees so git's `unknown rev` / `unable to read tree` diagnostics are
//!     reproduced; only the legacy three-tree walk with its embedded unified
//!     diff is not ported, as reproducing git's xdiff hunk framing byte-for-byte
//!     through `gix-imara-diff` cannot be guaranteed)
//!   * the strategy options `subtree[=<path>]`, `renormalize`, `patience`,
//!     `diff-algorithm=patience`, `ignore-space-change`, `ignore-all-space`,
//!     `ignore-space-at-eol` and `ignore-cr-at-eol` — `gix-merge`'s text driver
//!     exposes no whitespace-insensitive tokenizer, no renormalizing pipeline
//!     and no subtree shift, and `gix-imara-diff` has no patience algorithm.
//!     They parse and validate exactly as git does; only performing such a
//!     merge is refused.
//!   * message rendering for the remaining exotic conflict classes —
//!     directory/file, submodule and the rename type-mismatch failures.
//!     `gix-merge` reports these as structured resolutions whose exact git
//!     message text (including synthetic `~<label>` rename paths) cannot be
//!     reconstructed here. Those merges still work under `--no-messages` and
//!     `--quiet`, where no message text is emitted at all.

use anyhow::{bail, Result};
use std::io::{BufRead, Read, Write};
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::diff::tree_with_rewrites::Change;
use gix::hash::ObjectId;
use gix::merge::blob::builtin_driver::text::Labels;
use gix::merge::tree::apply_index_entries::RemovalMode;
use gix::merge::tree::{Conflict, FileFavor, Resolution, ResolutionFailure, TreatAsUnresolved};

/// The outcome of one real (`--write-tree`) merge, ready for framing by the
/// caller. `Fatal` carries the exit code git would `die()`/`exit()` with; it
/// aborts the whole process, including a `--stdin` batch mid-stream.
enum Merged {
    Fatal(ExitCode),
    Done { clean: bool, body: Vec<u8> },
}

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

/// One informational message, in both the human and the `-z` shape.
///
/// `paths` are git's `logical_conflict_info.paths` for this message: the first
/// entry is the *primary* path (git's strmap key, used to sort the messages),
/// and any further entries follow in git's `path_msg()` order (e.g. the source
/// then destination of a rename). `ctype` is git's stable short conflict type
/// (the `-z` field); `text` is the free-form line, which always carries its own
/// trailing newline exactly as git emits it via `puts()`.
struct Message {
    paths: Vec<BString>,
    ctype: &'static str,
    text: String,
}

/// The parsed `-X`/`--strategy-option` state, mirroring the subset of git's
/// `struct merge_options` that `parse_merge_opt()` can touch.
#[derive(Default)]
struct StrategyOptions {
    /// `ours` / `theirs`.
    favor: Option<FileFavor>,
    /// `subtree` (empty shift) or `subtree=<path>`.
    subtree: Option<String>,
    /// The requested diff algorithm, already normalized to lowercase.
    diff_algorithm: Option<String>,
    /// The first whitespace-insensitivity option seen, kept for the diagnostic.
    ignore_whitespace: Option<String>,
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
                    eprintln!("error: unknown option `{name}'");
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
        let repo = gix::discover(".")?;
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

            let tree_options = strategy.apply(repo.tree_merge_options()?)?;
            let mut outcome =
                match resolve_outcome(&repo, base, s1, s2, allow_unrelated, tree_options)? {
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

    let repo = gix::discover(".")?;

    if mode == Mode::Trivial {
        // git's trivial merge peels each of the three operands to a tree before
        // it walks them: an operand that names no object is a fatal `unknown rev`,
        // and one that names a non-tree is `unable to read tree`. Both fire
        // (exit 128) before the walk, so they are reproduced here even though the
        // legacy three-tree walk with its embedded unified diff is not ported.
        for spec in &revs {
            match resolve_tree(&repo, spec) {
                TreeResolution::Ok => {}
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
        bail!("unsupported mode \"--trivial-merge\" (git's deprecated three-tree walk is not ported; use --write-tree)");
    }

    let (spec1, spec2) = (revs[0].as_str(), revs[1].as_str());
    let tree_options = strategy.apply(repo.tree_merge_options()?)?;
    let mut outcome = match resolve_outcome(
        &repo,
        merge_base.as_deref(),
        spec1,
        spec2,
        allow_unrelated,
        tree_options,
    )? {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };

    let conflicted = outcome.has_unresolved_conflicts(TreatAsUnresolved::git());

    if quiet {
        // git suppresses all output here and only reports through the status.
        return Ok(exit_code(conflicted));
    }

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
fn resolve_outcome<'repo>(
    repo: &'repo gix::Repository,
    merge_base: Option<&str>,
    spec1: &str,
    spec2: &str,
    allow_unrelated: bool,
    tree_options: gix::merge::tree::Options,
) -> Result<std::result::Result<gix::merge::tree::Outcome<'repo>, ExitCode>> {
    let labels = Labels {
        ancestor: None,
        current: Some(BStr::new(spec1)),
        other: Some(BStr::new(spec2)),
    };
    // Both branches produce the same tree-merge outcome; only how the ancestor
    // is chosen differs.
    let outcome = if let Some(base_spec) = merge_base {
        // With an explicit base, git accepts plain trees for all three sides,
        // and reports any side that will not peel to one as a fatal error.
        let (Some(base), Some(ours), Some(theirs)) = (
            peel_tree(repo, base_spec),
            peel_tree(repo, spec1),
            peel_tree(repo, spec2),
        ) else {
            let bad = [base_spec, spec1, spec2]
                .into_iter()
                .find(|s| peel_tree(repo, s).is_none())
                .unwrap_or_default();
            eprintln!("fatal: could not parse as tree '{bad}'");
            return Ok(Err(ExitCode::from(128)));
        };
        repo.merge_trees(base, ours, theirs, labels, tree_options)?
    } else {
        let Some(ours) = peel_commit(repo, spec1) else {
            eprintln!("merge-tree: {spec1} - not something we can merge");
            return Ok(Err(ExitCode::FAILURE));
        };
        let Some(theirs) = peel_commit(repo, spec2) else {
            eprintln!("merge-tree: {spec2} - not something we can merge");
            return Ok(Err(ExitCode::FAILURE));
        };
        if !allow_unrelated && repo.merge_bases_many(ours, &[theirs])?.is_empty() {
            eprintln!("fatal: refusing to merge unrelated histories");
            return Ok(Err(ExitCode::from(128)));
        }
        let commit_options = gix::merge::commit::Options::from(tree_options)
            .with_allow_missing_merge_base(allow_unrelated);
        repo.merge_commits(ours, theirs, labels, commit_options)?
            .tree_merge
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
        let messages = render_messages(repo, &outcome.conflicts, label1, label2)?;
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
    fn absorb(&mut self, s: &str) -> bool {
        match s {
            "ours" => self.favor = Some(FileFavor::Ours),
            "theirs" => self.favor = Some(FileFavor::Theirs),
            "subtree" => self.subtree = Some(String::new()),
            "patience" => self.diff_algorithm = Some("patience".into()),
            "histogram" => self.diff_algorithm = Some("histogram".into()),
            "ignore-space-change"
            | "ignore-all-space"
            | "ignore-space-at-eol"
            | "ignore-cr-at-eol" => self.ignore_whitespace = Some(s.to_string()),
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
    fn apply(&self, options: gix::merge::tree::Options) -> Result<gix::merge::tree::Options> {
        if let Some(flag) = &self.ignore_whitespace {
            bail!("unsupported strategy option \"{flag}\" (gix-merge's text driver has no whitespace-insensitive tokenizer)");
        }
        if let Some(path) = &self.subtree {
            let shown = if path.is_empty() {
                "subtree".to_string()
            } else {
                format!("subtree={path}")
            };
            bail!("unsupported strategy option \"{shown}\" (gix-merge has no subtree shift)");
        }
        if self.renormalize == Some(true) {
            bail!("unsupported strategy option \"renormalize\" (gix-merge's blob pipeline is not driven in renormalizing mode here)");
        }

        let algorithm = match self.diff_algorithm.as_deref() {
            None => None,
            Some("myers" | "default") => Some(gix::diff::blob::Algorithm::Myers),
            Some("minimal") => Some(gix::diff::blob::Algorithm::MyersMinimal),
            Some("histogram") => Some(gix::diff::blob::Algorithm::Histogram),
            Some(other) => bail!(
                "unsupported strategy option \"{other}\" diff algorithm (gix-imara-diff implements myers, minimal and histogram only)"
            ),
        };

        // The rewrite and blob-merge knobs only exist on the plumbing options,
        // so round-trip through them before applying the builder-level ones.
        let mut plumbing: gix::merge::plumbing::tree::Options = options.into();
        if let Some(algorithm) = algorithm {
            plumbing.blob_merge.text.diff_algorithm = algorithm;
        }
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
    Some(
        repo.rev_parse_single(spec)
            .ok()?
            .object()
            .ok()?
            .peel_to_tree()
            .ok()?
            .id,
    )
}

/// Resolve `spec` to a commit id, or `None` when it is not something git would
/// accept as a side of the merge.
fn peel_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    Some(object.peel_to_commit().ok()?.id)
}

/// Outcome of resolving a trivial-merge operand, mirroring the two distinct
/// failure modes of git's `get_tree_descriptor()`.
enum TreeResolution {
    /// The spec peels to a tree.
    Ok,
    /// The spec names no object at all — git's `unknown rev`.
    UnknownRev,
    /// The spec resolves but does not peel to a tree — git's `unable to read
    /// tree`, which names the resolved object id.
    NotATree(ObjectId),
}

/// Resolve `spec` the way git's trivial merge does: to a tree, or one of the two
/// fatal conditions it distinguishes before it begins the three-tree walk.
fn resolve_tree(repo: &gix::Repository, spec: &str) -> TreeResolution {
    let id = match repo.rev_parse_single(spec) {
        Ok(id) => id,
        Err(_) => return TreeResolution::UnknownRev,
    };
    let object = match id.object() {
        Ok(object) => object,
        Err(_) => return TreeResolution::UnknownRev,
    };
    let oid = object.id;
    match object.peel_to_tree() {
        Ok(_) => TreeResolution::Ok,
        Err(_) => TreeResolution::NotATree(oid),
    }
}

/// Turn the structured conflict records into git's informational messages,
/// reproducing `merge-ort`'s `path_msg()` text byte-for-byte.
///
/// The content-merge family (`Auto-merging`/`CONFLICT (content|add/add)`), its
/// binary (`warning: Cannot merge binary files: …`) and symlink variants, and
/// the `modify/delete`, `rename/delete` and `rename/rename` tree conflicts are
/// rendered. `label1`/`label2` are git's `opt->branch1`/`opt->branch2`, i.e. the
/// two branch operands exactly as spelled on the command line; because
/// `gix-merge` normalizes *ours*/*theirs* independently of operand order, the
/// side each conflicting path belongs to is recovered from tree membership
/// (git derives the same labels positionally from `argv`).
///
/// The remaining exotic classes — directory/file, submodule, and the rename
/// type-mismatch failures — cannot have their git text reconstructed here and
/// are refused rather than approximated.
fn render_messages(
    repo: &gix::Repository,
    conflicts: &[Conflict],
    label1: &str,
    label2: &str,
) -> Result<Vec<Message>> {
    // The tree-conflict classes need each path mapped back to the operand it
    // came from. Peel both operand trees once, only when such a conflict is
    // present, so plain content merges pay nothing.
    let needs_labels = conflicts.iter().any(|c| {
        matches!(
            &c.resolution,
            Err(ResolutionFailure::OursModifiedTheirsDeleted
                | ResolutionFailure::OursDeletedTheirsRenamed
                | ResolutionFailure::OursRenamedTheirsRenamedDifferently { .. })
        )
    });
    let side_trees = if needs_labels {
        Some((
            repo.rev_parse_single(label1)?.object()?.peel_to_tree()?,
            repo.rev_parse_single(label2)?.object()?.peel_to_tree()?,
        ))
    } else {
        None
    };

    let mut out: Vec<Message> = Vec::new();
    for conflict in conflicts {
        let (ours, theirs) = conflict.changes_in_resolution();
        match &conflict.resolution {
            Ok(Resolution::OursModifiedTheirsModifiedThenBlobContentMerge { merged_blob }) => {
                let path = ours.location().to_owned();
                let conflicted =
                    merged_blob.resolution == gix::merge::blob::Resolution::Conflict;
                // Both sides adding the same path is reported as `add/add`; every
                // other content merge uses `content` (the `submodule` reason is
                // unreachable here, as gitlink merges are refused below).
                let reason = if matches!(ours, Change::Addition { .. })
                    && matches!(theirs, Change::Addition { .. })
                {
                    "add/add"
                } else {
                    "content"
                };
                let (our_mode, _) = change_state(ours);
                let (their_mode, _) = change_state(theirs);

                if our_mode.is_link() && their_mode.is_link() {
                    // Symlinks are content-merged via the binary driver, but
                    // `merge-ort`'s `S_ISLNK` arm emits no `Auto-merging` line —
                    // only the conflict notice, when the merge did not resolve.
                    if conflicted {
                        out.push(Message {
                            paths: vec![path.clone()],
                            ctype: "CONFLICT (contents)",
                            text: format!("CONFLICT ({reason}): Merge conflict in {path}\n"),
                        });
                    }
                } else if our_mode.is_blob() && their_mode.is_blob() {
                    // A binary content merge prints `warning: Cannot merge binary
                    // files` from inside `merge_3way()`, i.e. *before* the
                    // `Auto-merging` line that `handle_content_merge()` adds
                    // afterwards. The `(a vs. b)` labels are `opt->branch1`/`2`
                    // verbatim (no rename is involved in this variant, so git's
                    // `<branch>:<path>` form does not apply).
                    if conflicted && conflict_is_binary(repo, conflict)? {
                        out.push(Message {
                            paths: vec![path.clone()],
                            ctype: "CONFLICT (binary)",
                            text: format!(
                                "warning: Cannot merge binary files: {path} ({label1} vs. {label2})\n"
                            ),
                        });
                    }
                    out.push(Message {
                        paths: vec![path.clone()],
                        ctype: "Auto-merging",
                        text: format!("Auto-merging {path}\n"),
                    });
                    if conflicted {
                        out.push(Message {
                            paths: vec![path.clone()],
                            ctype: "CONFLICT (contents)",
                            text: format!("CONFLICT ({reason}): Merge conflict in {path}\n"),
                        });
                    }
                } else {
                    bail!(
                        "conflict at {path} is a submodule or type-mismatch content merge; its message text is not ported (retry with --no-messages or --quiet)"
                    );
                }
            }
            // Modify/delete: `changes_in_resolution()` orients `ours` to the
            // modified side and `theirs` to the deleted side. git names the two
            // by which operand still carries the file, which is exactly the tree
            // that retains `path`.
            Err(ResolutionFailure::OursModifiedTheirsDeleted) => {
                let (s1, _) = side_trees.as_ref().expect("peeled when this class is present");
                let path = ours.location().to_owned();
                let modify_branch = if tree_has(s1, path.as_bstr())? {
                    label1
                } else {
                    label2
                };
                let delete_branch = if modify_branch == label1 { label2 } else { label1 };
                out.push(Message {
                    paths: vec![path.clone()],
                    ctype: "CONFLICT (modify/delete)",
                    text: format!(
                        "CONFLICT (modify/delete): {path} deleted in {delete_branch} and modified in {modify_branch}.  Version {modify_branch} of {path} left in tree.\n"
                    ),
                });
            }
            // Rename/delete: `theirs` is the rename (a rewrite carrying source and
            // destination), `ours` the deletion. The renaming operand is the one
            // whose tree holds the new name.
            Err(ResolutionFailure::OursDeletedTheirsRenamed) => {
                let (s1, _) = side_trees.as_ref().expect("peeled when this class is present");
                let src = theirs.source_location().to_owned();
                let dst = theirs.location().to_owned();
                let rename_branch = if tree_has(s1, dst.as_bstr())? {
                    label1
                } else {
                    label2
                };
                let delete_branch = if rename_branch == label1 { label2 } else { label1 };
                out.push(Message {
                    // git's primary path is the new name, followed by the old one.
                    paths: vec![dst.clone(), src.clone()],
                    ctype: "CONFLICT (rename/delete)",
                    text: format!(
                        "CONFLICT (rename/delete): {src} renamed to {dst} in {rename_branch}, but deleted in {delete_branch}.\n"
                    ),
                });
            }
            // Rename/rename(1to2): both operands renamed the same source to
            // distinct destinations. git prints them positionally as `to <d1> in
            // <branch1> and to <d2> in <branch2>`, so `d1` is whichever
            // destination lives in operand 1's tree.
            Err(ResolutionFailure::OursRenamedTheirsRenamedDifferently { .. }) => {
                let (s1, _) = side_trees.as_ref().expect("peeled when this class is present");
                let src = ours.source_location().to_owned();
                let our_dst = ours.location().to_owned();
                let their_dst = theirs.location().to_owned();
                let (dst1, dst2) = if tree_has(s1, our_dst.as_bstr())? {
                    (our_dst, their_dst)
                } else {
                    (their_dst, our_dst)
                };
                out.push(Message {
                    // git's paths are the shared source, then both destinations.
                    paths: vec![src.clone(), dst1.clone(), dst2.clone()],
                    ctype: "CONFLICT (rename/rename)",
                    text: format!(
                        "CONFLICT (rename/rename): {src} renamed to {dst1} in {label1} and to {dst2} in {label2}.\n"
                    ),
                });
            }
            _ => bail!(
                "conflict at {} is a class whose git message text is not ported (retry with --no-messages or --quiet)",
                ours.location()
            ),
        }
    }

    // git accumulates messages in a strmap keyed by the primary path, then emits
    // them in sorted path order (`string_list_sort`), preserving insertion order
    // among messages that share a path. A stable sort by the primary path
    // reproduces that layout.
    out.sort_by(|a, b| a.paths[0].cmp(&b.paths[0]));
    Ok(out)
}

/// git's binary-merge trigger: `merge_3way()` emits its `warning:` line when
/// `ll_merge()` returns `LL_MERGE_BINARY_CONFLICT`, which happens when any of the
/// base/ours/theirs blobs is binary. Mirror that by testing every populated
/// stage of the conflict with git's NUL-in-first-8000-bytes heuristic.
fn conflict_is_binary(repo: &gix::Repository, conflict: &Conflict) -> Result<bool> {
    for entry in conflict.entries().into_iter().flatten() {
        if is_binary(repo, &entry.id)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `tree` contains an entry at the slash-separated `path`. Used to map a
/// conflicting path back to the operand it belongs to for git's branch labels.
fn tree_has(tree: &gix::Tree<'_>, path: &BStr) -> Result<bool> {
    Ok(tree.lookup_entry(path.split(|&b| b == b'/'))?.is_some())
}

/// The post-change mode and id of `change` (the rename destination for rewrites).
fn change_state(change: &Change) -> (gix::object::tree::EntryMode, ObjectId) {
    match change {
        Change::Addition { entry_mode, id, .. }
        | Change::Deletion { entry_mode, id, .. }
        | Change::Modification { entry_mode, id, .. }
        | Change::Rewrite { entry_mode, id, .. } => (*entry_mode, *id),
    }
}

/// git's binary heuristic: a NUL byte within the first 8000 bytes of the blob.
fn is_binary(repo: &gix::Repository, id: &ObjectId) -> Result<bool> {
    let data = repo.find_object(*id)?.data.clone();
    let head = &data[..data.len().min(8000)];
    Ok(head.contains(&0))
}

/// A path as it appears in the conflicted-file-info section: raw under `-z`,
/// otherwise C-quoted the way git's `core.quotePath` default does.
fn render_path(path: &BStr, nul: bool) -> Vec<u8> {
    if nul {
        path.to_vec()
    } else {
        quote_path(path).into_bytes()
    }
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        // All bytes are printable ASCII here, so this is lossless.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => {
                out.push_str(&format!("\\{b:03o}"));
            }
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}
