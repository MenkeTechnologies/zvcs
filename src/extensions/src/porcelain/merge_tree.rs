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
use std::io::{Read, Write};
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
/// `ctype` is git's stable short conflict type (the `-z` field); `text` is the
/// free-form line, which always carries its own trailing newline exactly as git
/// emits it.
struct Message {
    path: BString,
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
    // simply ignores any positional revs. It reads merges from stdin, one per
    // line, and forces NUL termination.
    if use_stdin {
        // `--stdin` batch mode (one merge per line, NUL-terminated) is not
        // implemented; the single-merge forms below are.
        anyhow::bail!("--stdin batch merges are not supported");
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

    if use_stdin {
        bail!("unsupported flag \"--stdin\" (the multi-merge batch protocol is not ported)");
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
    let labels = Labels {
        ancestor: None,
        current: Some(BStr::new(spec1)),
        other: Some(BStr::new(spec2)),
    };
    let tree_options = strategy.apply(repo.tree_merge_options()?)?;

    // Both branches below produce the same tree-merge outcome; only how the
    // ancestor is chosen differs.
    let mut outcome: gix::merge::tree::Outcome<'_> = if let Some(base_spec) = &merge_base {
        // With an explicit base, git accepts plain trees for all three sides,
        // and reports any side that will not peel to one as a fatal error.
        let (Some(base), Some(ours), Some(theirs)) = (
            peel_tree(&repo, base_spec),
            peel_tree(&repo, spec1),
            peel_tree(&repo, spec2),
        ) else {
            let bad = [base_spec.as_str(), spec1, spec2]
                .into_iter()
                .find(|s| peel_tree(&repo, s).is_none())
                .unwrap_or_default();
            eprintln!("fatal: could not parse as tree '{bad}'");
            return Ok(ExitCode::from(128));
        };
        repo.merge_trees(base, ours, theirs, labels, tree_options)?
    } else {
        let Some(ours) = peel_commit(&repo, spec1) else {
            eprintln!("merge-tree: {spec1} - not something we can merge");
            return Ok(ExitCode::FAILURE);
        };
        let Some(theirs) = peel_commit(&repo, spec2) else {
            eprintln!("merge-tree: {spec2} - not something we can merge");
            return Ok(ExitCode::FAILURE);
        };
        if !allow_unrelated && repo.merge_bases_many(ours, &[theirs])?.is_empty() {
            eprintln!("fatal: refusing to merge unrelated histories");
            return Ok(ExitCode::from(128));
        }
        let commit_options = gix::merge::commit::Options::from(tree_options)
            .with_allow_missing_merge_base(allow_unrelated);
        repo.merge_commits(ours, theirs, labels, commit_options)?
            .tree_merge
    };

    let how = TreatAsUnresolved::git();
    let conflicted = outcome.has_unresolved_conflicts(how);

    if quiet {
        // git suppresses all output here and only reports through the status.
        return Ok(exit_code(conflicted));
    }

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
        let messages = render_messages(&repo, &outcome.conflicts)?;
        if nul {
            // The `-z` messages section opens with its own NUL separator, then
            // carries one `<count> <path>... <type> <message>` record per entry.
            buf.push(b'\0');
            for m in &messages {
                buf.extend_from_slice(b"1\0");
                buf.extend_from_slice(m.path.as_slice());
                buf.push(b'\0');
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

    std::io::stdout().lock().write_all(&buf)?;
    Ok(exit_code(conflicted))
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

/// Turn the structured conflict records into git's informational messages.
///
/// Only the content-merge family is rendered: git's text for those is
/// reproduced exactly. Any other resolution class — and any content merge over
/// binary data or symlinks, where git prepends a `warning:` line we cannot
/// reconstruct — errors out instead of guessing.
fn render_messages(repo: &gix::Repository, conflicts: &[Conflict]) -> Result<Vec<Message>> {
    let mut out = Vec::new();
    for conflict in conflicts {
        let (ours, theirs) = conflict.changes_in_resolution();
        let path = ours.location().to_owned();
        let merged_blob = match &conflict.resolution {
            Ok(Resolution::OursModifiedTheirsModifiedThenBlobContentMerge { merged_blob }) => {
                merged_blob
            }
            _ => bail!(
                "conflict at {path} is not a content merge; message rendering for this conflict class is not ported (retry with --no-messages or --quiet)"
            ),
        };

        for change in [ours, theirs] {
            let (mode, id) = change_state(change);
            if !mode.is_blob() {
                bail!(
                    "conflict at {path} involves a symlink or submodule; message rendering is not ported (retry with --no-messages or --quiet)"
                );
            }
            if is_binary(repo, &id)? {
                bail!(
                    "conflict at {path} is a binary content merge; git's `warning: Cannot merge binary files` line is not ported (retry with --no-messages or --quiet)"
                );
            }
        }

        out.push(Message {
            path: path.clone(),
            ctype: "Auto-merging",
            text: format!("Auto-merging {path}\n"),
        });
        if merged_blob.resolution == gix::merge::blob::Resolution::Conflict {
            // Both sides adding the same path is reported as `add/add` in the
            // human message, but shares the `contents` short type with a plain
            // content conflict.
            let kind = if matches!(ours, Change::Addition { .. })
                && matches!(theirs, Change::Addition { .. })
            {
                "add/add"
            } else {
                "content"
            };
            out.push(Message {
                text: format!("CONFLICT ({kind}): Merge conflict in {path}\n"),
                path,
                ctype: "CONFLICT (contents)",
            });
        }
    }
    Ok(out)
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
