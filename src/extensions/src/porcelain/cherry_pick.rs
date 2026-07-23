//! `git cherry-pick <commit>...` — replay the change each commit introduces on
//! top of `HEAD`, recording a new commit for each.
//!
//! ## Order of operations
//!
//! git's `builtin/revert.c` runs a fixed pipeline, and the exit code a given
//! command line produces depends entirely on which stage it dies in. This port
//! reproduces that pipeline stage for stage, because a great many command lines
//! never reach the pick itself:
//!
//! 1. **Option parsing**, left to right. `--cleanup` is stored raw and only its
//!    *final* value is validated once parsing finishes (last-wins;
//!    `--no-cleanup` clears it), so `--cleanup=bogus --cleanup=default` is fine
//!    and `--cleanup=default --cleanup=bogus` dies. A bad final `--cleanup` mode
//!    dies with `fatal: Invalid cleanup mode <v>` and status 128 *here*, before
//!    anything else — including before the sequencer verbs, so
//!    `--cleanup=bogus --quit` is a failure even though `--quit` alone succeeds.
//!    `--empty` and `-m`, by contrast, validate *eagerly* per value as git's
//!    parse-options callbacks do, so a bad one of those (status 129) outranks a
//!    later bad `--cleanup`. A missing or malformed option value prints only its
//!    `error:` line and exits 129 without the usage block. Options git does not
//!    know are *kept*, not rejected (`PARSE_OPT_KEEP_UNKNOWN_OPT`), and only turn
//!    into an error in stage 5.
//! 2. **Sequencer verbs** (`--quit`, `--continue`, `--abort`, `--skip`). Options
//!    that would have no meaning alongside a verb are rejected with
//!    `fatal: cherry-pick: <opt> cannot be used with <verb>` and status 128.
//!    `--quit` with nothing in progress is a silent success; the other three
//!    report `no cherry-pick[ or revert] in progress` and exit 128.
//! 3. **No revisions given** → the usage block on stderr, status 129.
//! 4. **Revision resolution**, in argument order. The first spec that does not
//!    resolve dies with `fatal: bad revision '<spec>'` and status 128. This is
//!    why `--strategy=nonexistent-strategy README.md` reports the *revision*, not
//!    the strategy: nothing validates a strategy name before this point.
//! 5. **Leftover unknown options** → usage, status 129. A bad revision in stage 4
//!    outranks this, matching `setup_revisions`, which dies on the revision as it
//!    walks the argument list and only reports leftovers once it finishes.
//! 6. **Dirty worktree** → `fatal: cherry-pick failed`, status 128.
//! 7. The picks themselves.
//!
//! ## What is served
//!
//! Each pick is a three-way tree merge with *base* = the picked commit's
//! mainline parent (`-m`, default the first), *ours* = the current `HEAD` tree
//! and *theirs* = the picked commit's tree. A commit with no parents is replayed
//! against the empty tree, exactly as `do_pick_commit` does, and `-m` is not
//! consulted for it at all. The merge itself is `gix`'s tree merge, so renames
//! and hunk-level content merges are served rather than approximated.
//!
//! A pick that cannot be resolved stops the way git's does: the merge result —
//! conflict markers included — is checked out, the conflicting paths are given
//! stage 1/2/3 index entries, `CHERRY_PICK_HEAD`, `AUTO_MERGE` and a `MERGE_MSG`
//! carrying git's `# Conflicts:` hint are written, an `Auto-merging` and a
//! `CONFLICT (...)` line go to stdout, and the exit status is 1.
//!
//! `--continue`, `--skip` and `--abort` against a genuinely stopped pick are
//! still refused: this port can enter the stopped state but not resume from it,
//! which needs the `.git/sequencer` todo-list machinery.
//!
//! ## Empty results
//!
//! A pick whose merged tree equals the `HEAD` tree is *empty*, and git splits
//! that into two cases governed by different flags:
//!
//! - **Initially empty** (the picked commit and its parent have the same tree):
//!   committed when `--allow-empty` or `--empty=keep` is given, otherwise the
//!   pick stops. `--empty=drop` does *not* drop these.
//! - **Became empty** (the change is already upstream): committed under
//!   `--empty=keep` / `--keep-redundant-commits`, skipped with a
//!   `dropping <oid> <subject> -- patch contents already upstream` line on
//!   stderr under `--empty=drop`, and stops under the default `--empty=stop`.
//!
//! Stopping writes `CHERRY_PICK_HEAD`, `AUTO_MERGE` and `MERGE_MSG`, prints
//! git's cherry-pick-in-progress status block on stdout and its advice on
//! stderr, and exits 1 — matching stock git, whose worktree is necessarily clean
//! at that point, so the `nothing to commit, working tree clean` tail is
//! unconditional. `AUTO_MERGE` holds the merge result, which in this state is
//! `HEAD`'s own tree. git's `MERGE_RR` is not written: it is rerere bookkeeping,
//! and rerere does not participate here.
//!
//! ## Supported flags
//!
//! `-x`, `-m`/`--mainline`, `--ff`/`--no-ff`, `--allow-empty`,
//! `--allow-empty-message`, `--empty=stop|drop|keep`,
//! `--keep-redundant-commits`, `-s`/`--signoff`, `--cleanup=<mode>`,
//! `--no-edit`, `--commit`, the `--no-` forms of the above, `--quit`, and the
//! no-ops `-r`, `--no-gpg-sign` (we never sign) and `--rerere-autoupdate`
//! (rerere only participates in conflicts, which are refused anyway).
//!
//! Refused with a precise message: `-e`/`--edit`, `-n`/`--no-commit`,
//! `--strategy`, `-X`, `-S`, commit *ranges*, and `--continue`/`--skip`/
//! `--abort` against a pick that is genuinely in progress.
//!
//! Repository state for a successful pick matches git: the author signature
//! (name, email and time) is preserved from the picked commit, the committer
//! comes from configuration, and the reflog entry is `cherry-pick: <subject>`
//! (or `cherry-pick: fast-forward`). Mailmap is not applied to the printed
//! identities, so a repository that rewrites the picked author via `.mailmap`
//! would see a different ` Author:`. The detached-HEAD status header names the
//! current commit rather than the reflog-recorded detach point.
//!
//! The worktree-update helper below is a verbatim port of the one in
//! `porcelain::merge`; it cannot be shared because that module is private and
//! this port may only add a single file.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::objs::tree::EntryMode;
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// git's own `cherry-pick` usage block, verbatim, including the trailing blank
/// line. Printed on stderr for every 129 exit that is not a value error.
const USAGE: &str = "\
usage: git cherry-pick [--edit] [-n] [-m <parent-number>] [-s] [-x] [--ff]
                       [-S[<keyid>]] <commit>...
   or: git cherry-pick (--continue | --skip | --abort | --quit)

    --quit                end revert or cherry-pick sequence
    --continue            resume revert or cherry-pick sequence
    --abort               cancel revert or cherry-pick sequence
    --skip                skip current commit and continue
    --[no-]cleanup <mode> how to strip spaces and #comments from message
    -n, --no-commit       don't automatically commit
    --commit              opposite of --no-commit
    -e, --[no-]edit       edit the commit message
    -s, --[no-]signoff    add a Signed-off-by trailer
    -m, --[no-]mainline <parent-number>
                          select mainline parent
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    --[no-]strategy <strategy>
                          merge strategy
    -X, --[no-]strategy-option <option>
                          option for merge strategy
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG sign commit
    -x                    append commit name
    --[no-]ff             allow fast-forward
    --[no-]allow-empty    preserve initially empty commits
    --[no-]allow-empty-message
                          allow commits with empty messages
    --[no-]keep-redundant-commits
                          deprecated: use --empty=keep instead
    --empty (stop|drop|keep)
                          how to handle commits that become empty

";

/// The usage block on stderr, status 129 — git's `usage_with_options`.
fn usage() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// A single `error:` line on stderr, status 129 — git's `error()` returns from
/// an option callback, and `parse_options` exits without reprinting usage.
fn opt_error(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(129)
}

/// `fatal: <message>`, status 128 — git's `die()`.
fn fatal(message: &str) -> ExitCode {
    eprintln!("fatal: {message}");
    ExitCode::from(128)
}

/// git's sequencer failure shape: the specific `error:` line, then the generic
/// `fatal: cherry-pick failed`, status 128.
fn sequencer_failed(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    eprintln!("fatal: cherry-pick failed");
    ExitCode::from(128)
}

/// What to do with a pick whose result is empty (`--empty`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Empty {
    Stop,
    Drop,
    Keep,
}

/// `--cleanup` modes. `Default` resolves to `Whitespace` here because it only
/// differs from it when an editor runs, and `--edit` is refused.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cleanup {
    Verbatim,
    Whitespace,
    Strip,
    Scissors,
    Default,
}

/// The four sequencer verbs, carrying the spelling git uses in diagnostics.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    Quit,
    Continue,
    Abort,
    Skip,
}

impl Verb {
    fn name(self) -> &'static str {
        match self {
            Verb::Quit => "--quit",
            Verb::Continue => "--continue",
            Verb::Abort => "--abort",
            Verb::Skip => "--skip",
        }
    }
}

/// Everything the command line established, in the shape the pipeline consumes.
#[derive(Default)]
struct Opts<'a> {
    specs: Vec<&'a str>,
    /// An option git's table does not contain. Kept rather than rejected, and
    /// only fatal once every revision has resolved (stage 5).
    leftover_unknown: bool,
    verb: Option<Verb>,
    record_origin: bool,
    allow_ff: bool,
    allow_empty: bool,
    allow_empty_message: bool,
    no_commit: bool,
    edit: bool,
    signoff: bool,
    mainline: Option<u32>,
    empty: Option<Empty>,
    /// The raw, unvalidated `--cleanup` value (last-wins; `None` after
    /// `--no-cleanup`). git stores the string and validates only this final
    /// value once parsing is over, so a bad mode overwritten by a later good one
    /// never errors.
    cleanup: Option<&'a str>,
    strategy: bool,
    xopts: bool,
    gpg_sign: bool,
}

impl Opts<'_> {
    fn empty_action(&self) -> Empty {
        self.empty.unwrap_or(Empty::Stop)
    }
}

/// Parse `--cleanup`'s value the way git's `get_cleanup_mode` does: an unknown
/// mode is fatal (128) on the spot, not a usage error.
fn parse_cleanup(value: &str) -> Result<Cleanup, ExitCode> {
    match value {
        "verbatim" => Ok(Cleanup::Verbatim),
        "whitespace" => Ok(Cleanup::Whitespace),
        "strip" => Ok(Cleanup::Strip),
        "scissors" => Ok(Cleanup::Scissors),
        "default" => Ok(Cleanup::Default),
        other => Err(fatal(&format!("Invalid cleanup mode {other}"))),
    }
}

fn parse_empty(value: &str) -> Result<Empty, ExitCode> {
    match value {
        "stop" => Ok(Empty::Stop),
        "drop" => Ok(Empty::Drop),
        "keep" => Ok(Empty::Keep),
        other => Err(opt_error(&format!("invalid value for '--empty': '{other}'"))),
    }
}

/// `-m`'s value must be a positive integer; git rejects everything else with one
/// message regardless of whether it was unparsable or zero.
///
/// git reads it with `strtol()`, which *skips leading whitespace* and then
/// requires the remainder of the string to be consumed. That is load-bearing:
/// `-m 1` arriving as a single argument gives the callback `" 1"`, which stock
/// git accepts as 1. A plain `str::parse` rejects it.
fn parse_mainline(value: &str) -> Result<u32, ExitCode> {
    let trimmed = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits = trimmed.strip_prefix('+').unwrap_or(trimmed);
    match digits.parse::<u32>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(opt_error("option `mainline' expects a number greater than zero")),
    }
}

/// An option's value: either attached (`--opt=v`, `-Xv`) or the next argument.
/// Advances `i` when it consumes that next argument, like `parse_options` does.
fn take_value<'a>(
    attached: Option<&'a str>,
    args: &'a [String],
    i: &mut usize,
    name: &str,
) -> Result<&'a str, ExitCode> {
    match attached {
        Some(v) => Ok(v),
        None => {
            *i += 1;
            match args.get(*i) {
                Some(v) => Ok(v.as_str()),
                None => Err(opt_error(&format!("option `{name}' requires a value"))),
            }
        }
    }
}

/// Walk the command line once, left to right, exactly like `parse_options`.
///
/// Returns `Err(code)` for the errors that abort parsing immediately; unknown
/// options are recorded in `leftover_unknown` instead, so that a later bad
/// revision can outrank them.
fn parse<'a>(args: &'a [String]) -> Result<Opts<'a>, ExitCode> {
    let mut o = Opts::default();
    let mut only_specs = false;
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].as_str();
        if only_specs {
            o.specs.push(arg);
            i += 1;
            continue;
        }

        // Split `--name=value` once so both spellings share a match arm.
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (arg, None),
        };

        match name {
            "--" => only_specs = true,

            "--quit" => o.verb = o.verb.or(Some(Verb::Quit)),
            "--continue" => o.verb = o.verb.or(Some(Verb::Continue)),
            "--abort" => o.verb = o.verb.or(Some(Verb::Abort)),
            "--skip" => o.verb = o.verb.or(Some(Verb::Skip)),

            // Stored raw; the final value is validated post-parse, not here.
            "--cleanup" => {
                o.cleanup = Some(take_value(inline, args, &mut i, "cleanup")?);
            }
            "--no-cleanup" => o.cleanup = None,
            "--empty" => {
                o.empty = Some(parse_empty(take_value(inline, args, &mut i, "empty")?)?);
            }
            "--mainline" => {
                o.mainline = Some(parse_mainline(take_value(inline, args, &mut i, "mainline")?)?);
            }
            "--no-mainline" => o.mainline = None,
            "--strategy" => {
                take_value(inline, args, &mut i, "strategy")?;
                o.strategy = true;
            }
            "--no-strategy" => o.strategy = false,
            "--strategy-option" => {
                take_value(inline, args, &mut i, "strategy-option")?;
                o.xopts = true;
            }
            "--no-strategy-option" => o.xopts = false,
            "--gpg-sign" => o.gpg_sign = true,
            "--no-gpg-sign" => o.gpg_sign = false,

            "--commit" => o.no_commit = false,
            "--no-commit" => o.no_commit = true,
            "--edit" => o.edit = true,
            "--no-edit" => o.edit = false,
            "--signoff" => o.signoff = true,
            "--no-signoff" => o.signoff = false,
            "--ff" => o.allow_ff = true,
            "--no-ff" => o.allow_ff = false,
            "--allow-empty" => o.allow_empty = true,
            "--no-allow-empty" => o.allow_empty = false,
            "--allow-empty-message" => o.allow_empty_message = true,
            "--no-allow-empty-message" => o.allow_empty_message = false,
            "--keep-redundant-commits" => o.empty = Some(Empty::Keep),
            "--no-keep-redundant-commits" => o.empty = Some(Empty::Stop),
            // rerere only ever participates in conflict resolution, and every
            // conflicted pick is refused below, so both spellings are no-ops.
            "--rerere-autoupdate" | "--no-rerere-autoupdate" => {}

            _ if name.starts_with("--") => o.leftover_unknown = true,

            // Short options, including clusters like `-xn` and attached values
            // like `-Xtheirs` / `-m1` / `-S<keyid>`. A non-ASCII argument cannot
            // be a cluster of git's short options, so it goes straight to the
            // unknown pile rather than being sliced at a non-char boundary.
            _ if name.len() > 1 && name.starts_with('-') && name.is_ascii() => {
                let bytes = name.as_bytes();
                let mut c = 1;
                while c < bytes.len() {
                    let rest = &name[c + 1..];
                    let attached = (!rest.is_empty()).then_some(rest);
                    match bytes[c] {
                        b'x' => o.record_origin = true,
                        b'n' => o.no_commit = true,
                        b'e' => o.edit = true,
                        b's' => o.signoff = true,
                        b'm' => {
                            let v = take_value(attached, args, &mut i, "mainline")?;
                            o.mainline = Some(parse_mainline(v)?);
                            break;
                        }
                        b'X' => {
                            take_value(attached, args, &mut i, "strategy-option")?;
                            o.xopts = true;
                            break;
                        }
                        // `-S` takes an *optional* key id, so a bare `-S` never
                        // reaches into the next argument.
                        b'S' => {
                            o.gpg_sign = true;
                            break;
                        }
                        _ => {
                            o.leftover_unknown = true;
                            break;
                        }
                    }
                    c += 1;
                }
            }

            // Any other dashed argument: unknown, so kept for stage 5.
            _ if name.len() > 1 && name.starts_with('-') => o.leftover_unknown = true,

            _ => o.specs.push(arg),
        }
        i += 1;
    }

    Ok(o)
}

pub fn cherry_pick(args: &[String]) -> Result<ExitCode> {
    let opts = match parse(args) {
        Ok(o) => o,
        Err(code) => return Ok(code),
    };

    // --- stage 1 (tail): validate the final `--cleanup` value -------------
    // git's `parse_args` stores `--cleanup` raw and runs `get_cleanup_mode` on
    // the final value right after option parsing — before the sequencer verbs,
    // before revision resolution, before the no-revision usage error. A bad mode
    // dies `fatal: Invalid cleanup mode <v>` (128) here and nowhere later.
    let cleanup: Option<Cleanup> = match opts.cleanup {
        Some(raw) => match parse_cleanup(raw) {
            Ok(mode) => Some(mode),
            Err(code) => return Ok(code),
        },
        None => None,
    };

    // --- stage 2: sequencer verbs ----------------------------------------
    if let Some(verb) = opts.verb {
        return handle_verb(verb, &opts);
    }

    // --- stage 3: nothing to pick ----------------------------------------
    if opts.specs.is_empty() {
        return Ok(usage());
    }

    let repo = gix::discover(".")?;
    // The whole sequence (tree build, commit, HEAD move, worktree update) is one
    // logical write; hold the coordinator lock across all of it, like `merge`.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // --- stage 4: resolve every revision, in order ------------------------
    //
    // git's `setup_revisions` walks the whole argument list and dies on the
    // *first* spec that fails to resolve, regardless of what comes after it. So a
    // bad revision outranks the unsupported-range bail below: `main..feature
    // does-not-exist` must report `bad revision 'does-not-exist'` (128), not the
    // range. Validate every spec first; only after the list is clean does the
    // range become the failure.
    let mut picks: Vec<ObjectId> = Vec::with_capacity(opts.specs.len());
    let mut range_spec: Option<&str> = None;
    for spec in &opts.specs {
        let parsed = match repo.rev_parse(*spec) {
            Ok(parsed) => parsed,
            Err(_) => return Ok(fatal(&format!("bad revision '{spec}'"))),
        };
        match parsed.single() {
            // A revision may name an annotated tag (`v0.2.0`); git peels every
            // commit-ish it is handed before replaying it.
            Some(id) => picks.push(id.object()?.peel_to_commit()?.id),
            // Both endpoints resolved, so this is a genuine range. Enumerating it
            // is unported, and silently picking one end would be wrong — but the
            // bail waits until every remaining spec has been validated, so a
            // later bad revision still wins.
            None => range_spec = range_spec.or(Some(*spec)),
        }
    }
    if let Some(spec) = range_spec {
        anyhow::bail!(
            "commit ranges are not supported; name each commit individually (`{spec}`)"
        );
    }

    // --- stage 5: options git kept but never consumed ----------------------
    if opts.leftover_unknown {
        return Ok(usage());
    }

    if opts.edit {
        anyhow::bail!("`-e`/`--edit` (editor mode) is not supported");
    }
    if opts.no_commit {
        anyhow::bail!("`-n`/`--no-commit` is not supported; each pick is committed");
    }
    if opts.strategy || opts.xopts {
        anyhow::bail!("merge strategies are not supported; only trivially-resolvable picks are served");
    }
    if opts.gpg_sign {
        anyhow::bail!("commit signing is not supported");
    }

    let hash = repo.object_hash();

    let head = repo.head()?;
    if head.is_unborn() {
        anyhow::bail!("cannot cherry-pick onto an unborn branch");
    }
    let mut head_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    drop(head);

    // --- stage 6: git's `require_clean_work_tree` --------------------------
    if repo.is_dirty()? {
        eprintln!("error: your local changes would be overwritten by cherry-pick.");
        eprintln!("hint: commit your changes or stash them to proceed.");
        return Ok(sequencer_failed_tail());
    }

    // Committer identity, shared by every pick in the sequence.
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("committer identity is not configured"))??;
    let committer_ident = format!("{} <{}>", committer.name, committer.email);

    let should_interrupt = AtomicBool::new(false);
    // Index mirroring the current (clean) worktree; carried forward across picks
    // so filesystem stats are reused and a later `status` stays cheap.
    let mut index = repo.index_or_load_from_head()?.into_owned();

    for (spec, pick_id) in opts.specs.iter().zip(picks) {
        let pick = repo.find_commit(pick_id)?;

        // --- pick the base (mainline) parent ------------------------------
        let parents: Vec<ObjectId> = pick.parent_ids().map(|id| id.detach()).collect();
        let base_id: Option<ObjectId> = if parents.is_empty() {
            // git's `do_pick_commit` tests `!commit->parents` *first*: a root
            // commit is replayed against the empty tree and `-m` is never
            // consulted, so `-m 1 <root>` is accepted rather than rejected.
            None
        } else {
            match opts.mainline {
                // `-m N` is legal on a non-merge as long as parent N exists, so a
                // plain commit accepts `-m 1` and behaves exactly as without it.
                Some(n) => match parents.get(n as usize - 1) {
                    Some(id) => Some(*id),
                    None => {
                        return Ok(sequencer_failed(&format!(
                            "commit {pick_id} does not have parent {n}"
                        )));
                    }
                },
                None => match parents.len() {
                    1 => Some(parents[0]),
                    _ => {
                        return Ok(sequencer_failed(&format!(
                            "commit {pick_id} is a merge but no -m option was given."
                        )));
                    }
                },
            }
        };

        let pick_tree = pick.tree_id()?.detach();
        let base_tree = match base_id {
            Some(id) => repo.find_commit(id)?.tree_id()?.detach(),
            None => ObjectId::empty_tree(hash),
        };
        let head_tree = repo.find_commit(head_id)?.tree_id()?.detach();

        // `--ff`: HEAD is exactly the picked commit's parent, so replaying the
        // change is a fast-forward. git prints nothing in this case.
        if opts.allow_ff && base_id == Some(head_id) {
            advance_head(&repo, head_id, pick_id, "cherry-pick: fast-forward".into())?;
            index = update_clean_worktree(&repo, &index, pick_tree, &should_interrupt)?;
            head_id = pick_id;
            continue;
        }

        // --- three-way tree merge -----------------------------------------
        //
        // Labels come from `get_message()` in git's `sequencer.c`: *ours* is the
        // literal `HEAD`, *theirs* is `<abbrev> (<subject>)`, and the ancestor is
        // that same string prefixed with `parent of `. They are what ends up in
        // the `<<<<<<<` / `>>>>>>>` marker lines, so they are computed from the
        // picked commit's *own* message, before `-x`/`--signoff` rewrite it.
        let pick_subject = gix::objs::commit::MessageRef::from_bytes(pick.message_raw()?)
            .summary()
            .to_str_lossy()
            .into_owned();
        let pick_short = pick_id.attach(&repo).shorten_or_id().to_string();
        let other_label = format!("{pick_short} ({pick_subject})");
        let ancestor_label = format!("parent of {other_label}");

        let mut merge = repo.merge_trees(
            base_tree,
            head_tree,
            pick_tree,
            gix::merge::blob::builtin_driver::text::Labels {
                ancestor: Some(BStr::new(ancestor_label.as_bytes())),
                current: Some(BStr::new("HEAD")),
                other: Some(BStr::new(other_label.as_bytes())),
            },
            repo.tree_merge_options()?,
        )?;
        let tree_id = merge.tree.write()?.detach();

        // git's merge-ort emits messages grouped per path: an `Auto-merging`
        // line for every attempted blob merge, then a `CONFLICT (...)` line for
        // the ones it could not resolve. Identical changes on both sides resolve
        // trivially and are reported by neither, which is why picking a commit
        // that is already applied stays silent.
        let unresolved = gix::merge::tree::TreatAsUnresolved::git();
        let mut conflicted: Vec<BString> = Vec::new();
        for conflict in &merge.conflicts {
            let path = conflict.changes_in_resolution().0.location().to_owned();
            if conflict.content_merge().is_some() {
                println!("Auto-merging {path}");
            }
            if !conflict.is_unresolved(unresolved) {
                continue;
            }
            // merge-ort's `filemask == 6`: no ancestor stage means both sides
            // added the path, which it reports as `add/add` rather than
            // `content`.
            let kind = if conflict.entries()[0].is_none() {
                "add/add"
            } else {
                "content"
            };
            println!("CONFLICT ({kind}): Merge conflict in {path}");
            conflicted.push(path);
        }

        // The merged tree is the state of every path after the merge, conflict
        // markers included; the diffstat and the empty-result test both read it
        // back rather than tracking resolutions as they are made.
        let mut resolved: Vec<(BString, EntryMode, ObjectId)> = flatten(&repo, tree_id)?
            .into_iter()
            .map(|(path, (mode, id))| (path, mode, id))
            .collect();
        resolved.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        // --- message -----------------------------------------------------
        let mut message: BString = pick.message_raw()?.to_owned();
        // Only an explicit `--cleanup` rewrites the picked message; without one
        // git carries it across untouched.
        if let Some(mode) = cleanup {
            message = cleanup_message(&message, mode);
        }
        if message.trim().is_empty() && !opts.allow_empty_message {
            anyhow::bail!("the commit message of {spec} is empty (use --allow-empty-message)");
        }
        if message.last() != Some(&b'\n') {
            message.push(b'\n');
        }
        if opts.record_origin {
            if !has_conforming_footer(&message) {
                message.push(b'\n');
            }
            message.extend_from_slice(b"(cherry picked from commit ");
            message.extend_from_slice(pick_id.to_string().as_bytes());
            message.extend_from_slice(b")\n");
        }
        if opts.signoff {
            let trailer = format!("Signed-off-by: {committer_ident}\n");
            if !message.ends_with(trailer.as_bytes()) {
                if !has_conforming_footer(&message) {
                    message.push(b'\n');
                }
                message.extend_from_slice(trailer.as_bytes());
            }
        }
        let subject = gix::objs::commit::MessageRef::from_bytes(message.as_bstr())
            .summary()
            .to_str_lossy()
            .into_owned();

        // --- stopped on conflict ------------------------------------------
        //
        // Checked before the empty-result guards, as git does: a conflicted pick
        // never reaches the point where emptiness would be decided.
        if !conflicted.is_empty() {
            let mut new_index =
                update_clean_worktree(&repo, &index, tree_id, &should_interrupt)?;
            merge.index_changed_after_applying_conflicts(
                &mut new_index,
                unresolved,
                gix::merge::tree::apply_index_entries::RemovalMode::Prune,
            );
            new_index.write(Default::default())?;

            let git_dir = repo.git_dir();
            std::fs::write(git_dir.join("CHERRY_PICK_HEAD"), format!("{pick_id}\n"))?;
            // git records the merge result — conflict markers and all — as
            // `AUTO_MERGE` so `--continue` can diff against it later.
            std::fs::write(git_dir.join("AUTO_MERGE"), format!("{tree_id}\n"))?;

            // git's `append_conflicts_hint`: a blank line, then one commented
            // line per conflicted path, appended to the message it would have
            // committed.
            let mut merge_msg = message.clone();
            merge_msg.push(b'\n');
            merge_msg.extend_from_slice(b"# Conflicts:\n");
            for path in &conflicted {
                merge_msg.extend_from_slice(b"#\t");
                merge_msg.extend_from_slice(&path[..]);
                merge_msg.push(b'\n');
            }
            std::fs::write(git_dir.join("MERGE_MSG"), &merge_msg[..])?;

            eprintln!("error: could not apply {pick_short}... {pick_subject}");
            eprintln!("hint: After resolving the conflicts, mark them with");
            eprintln!("hint: \"git add/rm <pathspec>\", then run");
            eprintln!("hint: \"git cherry-pick --continue\".");
            eprintln!("hint: You can instead skip this commit with \"git cherry-pick --skip\".");
            eprintln!("hint: To abort and get back to the state before \"git cherry-pick\",");
            eprintln!("hint: run \"git cherry-pick --abort\".");
            return Ok(ExitCode::from(1));
        }

        // --- empty-result guards -----------------------------------------
        if tree_id == head_tree {
            let initially_empty = pick_tree == base_tree;
            let action = opts.empty_action();
            let keep = action == Empty::Keep || (initially_empty && opts.allow_empty);
            if !keep {
                // `--empty` governs commits that *became* empty; an initially
                // empty one is only ever kept via `--allow-empty`/`--empty=keep`.
                if !initially_empty && action == Empty::Drop {
                    eprintln!(
                        "dropping {pick_id} {subject} -- patch contents already upstream"
                    );
                    continue;
                }
                return stop_empty(&repo, pick_id, head_id, tree_id, &message);
            }
        }

        // --- write the commit, preserving the original author -------------
        let author = pick.author()?;
        let author_ident = format!("{} <{}>", author.name, author.email);
        let author_time = author.time()?;

        let commit = gix::objs::Commit {
            message: message.clone(),
            tree: tree_id,
            author: author.to_owned()?,
            committer: committer.to_owned()?,
            encoding: None,
            parents: std::iter::once(head_id).collect(),
            extra_headers: Default::default(),
        };
        let new_id = repo.write_object(&commit)?.detach();

        let reflog = gix::reference::log::message("cherry-pick", message.as_bstr(), 1);
        advance_head(&repo, head_id, new_id, reflog)?;
        index = update_clean_worktree(&repo, &index, tree_id, &should_interrupt)?;

        // --- summary, matching git's `print_commit_summary` ----------------
        let branch_label = match repo.head_name()? {
            Some(name) => name.shorten().to_string(),
            None => "detached HEAD".to_string(),
        };
        println!("[{branch_label} {}] {subject}", new_id.attach(&repo).shorten_or_id());
        if author_ident != committer_ident {
            println!(" Author: {author_ident}");
        }
        {
            use gix::date::time::format;
            println!(" Date: {}", author_time.format_or_unix(format::DEFAULT));
        }
        print_diffstat(&repo, head_tree, tree_id, &resolved)?;

        head_id = new_id;
    }

    Ok(ExitCode::SUCCESS)
}

/// The bare `fatal: cherry-pick failed` tail, for call sites that already
/// printed their own `error:` line(s).
fn sequencer_failed_tail() -> ExitCode {
    eprintln!("fatal: cherry-pick failed");
    ExitCode::from(128)
}

/// Stage 2 of git's pipeline: a sequencer verb was given.
fn handle_verb(verb: Verb, opts: &Opts<'_>) -> Result<ExitCode> {
    // git's `verify_opt_compatible`, in its declaration order.
    for (name, given) in [
        ("--no-commit", opts.no_commit),
        ("--signoff", opts.signoff),
        ("--mainline", opts.mainline.is_some()),
        ("--strategy", opts.strategy),
        ("--strategy-option", opts.xopts),
        ("-x", opts.record_origin),
        ("--ff", opts.allow_ff),
    ] {
        if given {
            return Ok(fatal(&format!(
                "cherry-pick: {name} cannot be used with {}",
                verb.name()
            )));
        }
    }

    let repo = gix::discover(".")?;
    let git_dir = repo.git_dir().to_owned();
    let in_progress =
        git_dir.join("CHERRY_PICK_HEAD").exists() || git_dir.join("sequencer").exists();

    Ok(match verb {
        // `--quit` forgets the operation without touching index or worktree, and
        // succeeds even when nothing was in progress.
        Verb::Quit => {
            let _ = std::fs::remove_file(git_dir.join("CHERRY_PICK_HEAD"));
            let _ = std::fs::remove_file(git_dir.join("MERGE_MSG"));
            let _ = std::fs::remove_file(git_dir.join("AUTO_MERGE"));
            let _ = std::fs::remove_dir_all(git_dir.join("sequencer"));
            ExitCode::SUCCESS
        }
        Verb::Skip if !in_progress => {
            sequencer_failed("no cherry-pick in progress")
        }
        Verb::Continue | Verb::Abort if !in_progress => {
            sequencer_failed("no cherry-pick or revert in progress")
        }
        // A pick really is stopped, but resuming needs the staged-conflict and
        // `.git/sequencer` machinery this port does not write.
        other => anyhow::bail!(
            "a cherry-pick is in progress, but resuming it (`{}`) is not implemented",
            other.name()
        ),
    })
}

/// git's stopped-on-empty state: record `CHERRY_PICK_HEAD`, `AUTO_MERGE` and
/// `MERGE_MSG`, print the in-progress status block on stdout and the advice on
/// stderr, exit 1.
///
/// The worktree is necessarily clean here — the merged tree equalled `HEAD`'s —
/// so the status block's body is fixed rather than derived from a diff. git still
/// records the merge result as `AUTO_MERGE`, which in this state is that same
/// tree; `merged_tree` is passed rather than re-derived so the file says what was
/// actually merged.
fn stop_empty(
    repo: &gix::Repository,
    pick_id: ObjectId,
    head_id: ObjectId,
    merged_tree: ObjectId,
    message: &BString,
) -> Result<ExitCode> {
    let git_dir = repo.git_dir();
    std::fs::write(git_dir.join("CHERRY_PICK_HEAD"), format!("{pick_id}\n"))?;
    std::fs::write(git_dir.join("AUTO_MERGE"), format!("{merged_tree}\n"))?;
    std::fs::write(git_dir.join("MERGE_MSG"), &message[..])?;

    match repo.head_name()? {
        Some(name) => println!("On branch {}", name.shorten()),
        None => println!("HEAD detached at {}", head_id.attach(repo).shorten_or_id()),
    }
    println!(
        "You are currently cherry-picking commit {}.",
        pick_id.attach(repo).shorten_or_id()
    );
    println!("  (all conflicts fixed: run \"git cherry-pick --continue\")");
    println!("  (use \"git cherry-pick --skip\" to skip this patch)");
    println!("  (use \"git cherry-pick --abort\" to cancel the cherry-pick operation)");
    println!();
    println!("nothing to commit, working tree clean");

    eprintln!("The previous cherry-pick is now empty, possibly due to conflict resolution.");
    eprintln!("If you wish to commit it anyway, use:");
    eprintln!();
    eprintln!("    git commit --allow-empty");
    eprintln!();
    eprintln!("Otherwise, please use 'git cherry-pick --skip'");

    Ok(ExitCode::from(1))
}

/// git's `strbuf_stripspace` plus the per-mode extras: trailing whitespace goes,
/// runs of blank lines collapse to one, and leading/trailing blank lines are
/// dropped. `strip` additionally removes comment lines, `scissors` truncates at
/// the scissors marker, and `verbatim` returns the message untouched.
fn cleanup_message(message: &BString, mode: Cleanup) -> BString {
    if mode == Cleanup::Verbatim {
        return message.clone();
    }
    let strip_comments = mode == Cleanup::Strip;

    let mut out: Vec<u8> = Vec::with_capacity(message.len());
    let mut pending_blank = false;
    let mut seen_content = false;
    for line in message.split(|&b| b == b'\n') {
        // `# ------------------------ >8 ------------------------` and
        // everything after it is the scissors cut.
        if mode == Cleanup::Scissors && line.starts_with(b"# ") && line.contains_str(">8") {
            break;
        }
        if strip_comments && line.first() == Some(&b'#') {
            continue;
        }
        let trimmed = match line.iter().rposition(|b| !b.is_ascii_whitespace()) {
            Some(last) => &line[..=last],
            None => &[][..],
        };
        if trimmed.is_empty() {
            pending_blank = seen_content;
            continue;
        }
        if pending_blank {
            out.push(b'\n');
            pending_blank = false;
        }
        out.extend_from_slice(trimmed);
        out.push(b'\n');
        seen_content = true;
    }
    BString::from(out)
}

/// Flatten a tree into `path -> (mode, blob id)`, recursively, dropping entries
/// whose mode has no tree representation (which cannot occur for a real tree).
fn flatten(
    repo: &gix::Repository,
    tree: ObjectId,
) -> Result<HashMap<BString, (EntryMode, ObjectId)>> {
    let index = repo.index_from_tree(&tree)?;
    let backing = index.path_backing();
    let mut out = HashMap::with_capacity(index.entries().len());
    for e in index.entries() {
        if let Some(mode) = e.mode.to_tree_entry_mode() {
            out.insert(e.path_in(backing).to_owned(), (mode, e.id));
        }
    }
    Ok(out)
}

/// Point `HEAD` (writing through to its branch when attached) at `new`, with
/// `reflog` as the reflog message, requiring `old` to still be the current tip.
fn advance_head(
    repo: &gix::Repository,
    old: ObjectId,
    new: ObjectId,
    reflog: BString,
) -> Result<()> {
    let name: FullName = "HEAD"
        .try_into()
        .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?;
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: reflog,
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(old)),
            new: Target::Object(new),
        },
        name,
        deref: true,
    })?;
    Ok(())
}

/// git's short-stat plus the create/delete/mode-change block, for the diff from
/// `old_tree` to `new_tree`. Ported from `porcelain::commit`, which derives the
/// file count from the flattened entry sets (so a rename counts as a delete plus
/// a create) and the line counts from a rename-free tree diff.
fn print_diffstat(
    repo: &gix::Repository,
    old_tree: ObjectId,
    new_tree: ObjectId,
    new_entries: &[(BString, EntryMode, ObjectId)],
) -> Result<()> {
    let old_entries = flatten(repo, old_tree)?;
    let new_paths: HashSet<&BString> = new_entries.iter().map(|(p, _, _)| p).collect();

    let mut files_changed: u64 = 0;
    let mut summary: Vec<(BString, String)> = Vec::new();
    for (path, mode, id) in new_entries {
        match old_entries.get(path) {
            None => {
                files_changed += 1;
                summary.push((path.clone(), format!("create mode {} {path}", octal(*mode))));
            }
            Some((old_mode, old_id)) => {
                if old_id != id || old_mode != mode {
                    files_changed += 1;
                }
                if old_mode != mode {
                    summary.push((
                        path.clone(),
                        format!("mode change {} => {} {path}", octal(*old_mode), octal(*mode)),
                    ));
                }
            }
        }
    }
    for (path, (mode, _)) in &old_entries {
        if !new_paths.contains(path) {
            files_changed += 1;
            summary.push((path.clone(), format!("delete mode {} {path}", octal(*mode))));
        }
    }

    if files_changed == 0 {
        return Ok(());
    }

    let old = repo.find_tree(old_tree)?;
    let new = repo.find_tree(new_tree)?;
    let mut platform = old.changes()?;
    platform.options(|opts| {
        opts.track_rewrites(None);
    });
    let stats = platform.stats(&new)?;

    let (ins, del) = (stats.lines_added, stats.lines_removed);
    let mut line = format!(" {files_changed} file{} changed", plural(files_changed));
    if ins > 0 || del == 0 {
        line.push_str(&format!(", {ins} insertion{}(+)", plural(ins)));
    }
    if del > 0 || ins == 0 {
        line.push_str(&format!(", {del} deletion{}(-)", plural(del)));
    }
    println!("{line}");

    summary.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, l) in &summary {
        println!(" {l}");
    }
    Ok(())
}

/// git's `has_conforming_footer`: does the message end in a trailer block, so
/// that `-x` may append its line without inserting a blank line first?
///
/// The block is the last paragraph, and it only qualifies when it is not also
/// the first paragraph (a subject is never a footer) and every one of its lines
/// is a `token: value` trailer, an indented continuation, a `#` comment, or a
/// `(cherry picked from commit <id>)` line — the last form being what makes a
/// second `-x` append directly, as stock git does.
fn has_conforming_footer(msg: &[u8]) -> bool {
    // Drop trailing blank lines.
    let mut lines: Vec<&[u8]> = msg.split(|&b| b == b'\n').collect();
    while lines.last().is_some_and(|l| l.iter().all(u8::is_ascii_whitespace)) {
        lines.pop();
    }
    if lines.is_empty() {
        return false;
    }

    // Start of the last paragraph.
    let mut start = lines.len();
    while start > 0 && !lines[start - 1].iter().all(u8::is_ascii_whitespace) {
        start -= 1;
    }
    // No preceding blank line means this paragraph is the subject.
    if start == 0 {
        return false;
    }

    let mut saw_trailer = false;
    for line in &lines[start..] {
        if line.first().is_some_and(|b| b.is_ascii_whitespace()) || line.starts_with(b"#") {
            continue;
        }
        if line.starts_with(b"(cherry picked from commit ") && line.ends_with(b")") {
            saw_trailer = true;
            continue;
        }
        match line.iter().position(|&b| b == b':') {
            // A trailer token is non-empty and contains no whitespace.
            Some(sep)
                if sep > 0 && !line[..sep].iter().any(u8::is_ascii_whitespace) =>
            {
                saw_trailer = true;
            }
            _ => return false,
        }
    }
    saw_trailer
}

/// Move a clean worktree and its index from the state captured in `old` to
/// `new_tree_id`, writing only the files that changed, and return the index that
/// was persisted.
///
/// Verbatim port of `porcelain::merge`'s helper: added/modified files are
/// checked out via `gix-worktree-state`, removed files are deleted, and the new
/// index reuses prior stats for unchanged entries.
fn update_clean_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_tree_id: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<gix::index::File> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    let mut new_index = repo.index_from_tree(&new_tree_id)?;
    let mut subset = repo.index_from_tree(&new_tree_id)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        None => false,
    });

    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    let discard_files = gix::progress::Discard;
    let discard_bytes = gix::progress::Discard;
    crate::worktree::checkout_subset(
        &mut subset,
        workdir.as_path(),
        odb,
        &discard_files,
        &discard_bytes,
        should_interrupt,
        opts,
    )?;

    let new_paths: HashSet<BString> = {
        let backing = new_index.path_backing();
        new_index
            .entries()
            .iter()
            .map(|e| e.path_in(backing).to_owned())
            .collect()
    };
    {
        let backing = old.path_backing();
        for e in old.entries() {
            let path = e.path_in(backing);
            if !new_paths.contains(&path.to_owned()) {
                if let Some(full) = repo.workdir_path(path) {
                    let _ = std::fs::remove_file(full);
                }
            }
        }
    }

    let mut subset_stats: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            subset_stats.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

    {
        let backing = new_index.path_backing().to_owned();
        for e in new_index.entries_mut() {
            let path = e.path_in(&backing).to_owned();
            if let Some(stat) = subset_stats.get(&path) {
                e.stat = *stat;
            } else if let Some((oid, mode, stat)) = old_map.get(&path) {
                if *oid == e.id && *mode == e.mode {
                    e.stat = *stat;
                }
            }
        }
    }

    new_index.remove_tree();
    new_index.write(Default::default())?;
    Ok(new_index)
}

/// The git-internal octal representation of a tree entry mode, e.g. `100644`.
fn octal(mode: EntryMode) -> String {
    let mut buf = [0u8; 6];
    mode.as_bytes(&mut buf).to_string()
}

/// `""` for a count of 1, `"s"` otherwise — for git's `file`/`files` etc.
fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
