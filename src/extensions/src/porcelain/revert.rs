//! `git revert <commit>...` — record new commits that undo earlier ones.
//!
//! A revert is a three-way merge with the roles rotated: the *base* is the tree
//! of the commit being reverted, *ours* is the current `HEAD` tree, and *theirs*
//! is the tree of that commit's parent. Applying it yields a tree in which the
//! reverted commit's changes are backed out while later work is preserved.
//!
//! The option grammar mirrors git's two-phase handling, because the order in
//! which git rejects things is observable:
//!
//!   * **parse phase**, left to right — `-m`/`--mainline` validates its value
//!     immediately (`expects a number greater than zero`, exit 129), and the
//!     `--quit`/`--continue`/`--abort`/`--skip` command modes reject a second,
//!     different mode on the spot (exit 129). Unknown options are *kept*, as
//!     git does, so they are not diagnosed until the operand phase.
//!   * **post-parse phase**, in a fixed order — `--cleanup` mode validation
//!     (`fatal: Invalid cleanup mode <arg>`, exit 128), then the
//!     command-mode/option compatibility list, then the "a command mode takes
//!     no operands" usage error, then "no operands at all" usage error.
//!
//! That ordering is why `--cleanup=bogus --mainline=0` reports the mainline
//! problem while `--cleanup=bogus --abort -n` reports the cleanup one.
//!
//! What this port covers, byte-for-byte against stock git:
//!   * `git revert <commit>...`, including `<a>..<b>` ranges and `^<commit>`
//!     exclusions, resolved through one revision walk as git's sequencer does
//!   * `-n`/`--no-commit`, `-s`/`--signoff`, `-m <n>`/`--mainline <n>`,
//!     `--no-edit`, `--reference`, `--cleanup=<mode>`
//!   * `--strategy`/`-X`, which git's sequencer ignores outright for a revert:
//!     `do_pick_commit` routes `TODO_REVERT` to the recursive merge regardless
//!     of the selected strategy, so an unknown strategy name is not an error
//!   * `--rerere-autoupdate`/`--no-rerere-autoupdate` and `--no-gpg-sign`,
//!     accepted and without effect on a conflict-free revert
//!   * the generated message (`Revert "…"` / `Reapply "…"`, the reference
//!     format `# *** SAY WHY … ***` plus `<short> (<subject>, <date>)`, the
//!     `, reversing / changes made to` merge variant, the `Signed-off-by`
//!     trailer) and the `--cleanup` mode applied to it
//!   * the summary block (`[<branch> <short-oid>] <subject>`, the ` Date:` line
//!     the sequencer always prints, the short-stat — gitlink changes included,
//!     which the blob differ cannot see — and create/delete/mode lines)
//!   * `--no-commit` merging against the index rather than `HEAD`, so a
//!     pre-existing staged change is carried through and repeated `-n` steps
//!     stack; the index tree and the merge result tree are written to the object
//!     database before the checkout, as git's do, so even a refused `-n` revert
//!     leaves the same objects behind
//!   * the `revert: <subject>` reflog message, and the `REVERT_HEAD`,
//!     `MERGE_MSG` and `AUTO_MERGE` files written by `--no-commit`
//!   * the refusal paths in git's own order: bad revision, an unmerged index,
//!     an index that differs from `HEAD`, merge without `-m`, missing parent,
//!     and affected files that are locally modified or would clobber untracked
//!     files — same text, same exit codes (128/129)
//!   * `--quit` (drops `.git/sequencer`), and the "nothing in progress"
//!     refusals of `--abort`/`--skip`/`--continue`
//!
//! What this port does NOT do, and refuses rather than approximates:
//!   * **content-level (blob) three-way merge.** The vendored `gix` in this
//!     crate is built without the `merge` feature, so `gix-merge` — both the
//!     blob and the tree merge drivers — is not linked in. Only the trivial
//!     resolutions are performed here: a path taken from *theirs* when *ours*
//!     never moved off the base, from *ours* when *theirs* never moved, or the
//!     shared value when both agree. A path that both sides changed away from
//!     the base needs a real blob merge and is refused. Rename detection is part
//!     of the same missing substrate, so a reverted path that was later renamed
//!     lands in that refused set too.
//!   * resuming a sequence: `--continue`, and `--abort`/`--skip` when a revert
//!     really is in progress, all need `git reset --merge`, which is not ported.
//!     A multi-pick (sequencer) revert whose pick empties out prints git's
//!     `Revert currently in progress` status block byte-for-byte, but the
//!     `.git/sequencer` todo/opts backing that block is not written, since
//!     nothing here can resume it — the object set, index, refs and worktree
//!     an empty pick leaves are already identical to git's.
//!   * `-S`/`--gpg-sign` — bails, since nothing here can produce a signature.
//!   * **spawning an editor.** `-e`/`--edit` is accepted and only changes which
//!     `--cleanup=default` mode applies; the generated message is then taken as
//!     written. Under a non-interactive `GIT_EDITOR` that is what git does too,
//!     but at a terminal git would prompt and this will not.

use anyhow::{bail, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode, Stat};
use gix::objs::tree::{EntryKind, EntryMode};
use gix::prelude::ObjectIdExt;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::Target;

/// Verbatim `git revert -h` text, printed on a usage error exactly as git does.
const USAGE: &str = "\
usage: git revert [--[no-]edit] [-n] [-m <parent-number>] [-s] [-S[<keyid>]] <commit>...
   or: git revert (--continue | --skip | --abort | --quit)

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
    --[no-]reference      use the 'reference' format to refer to commits
";

/// The title git puts on a `--reference` revert, left for the user to replace.
const REFERENCE_TITLE: &str = "# *** SAY WHY WE ARE REVERTING ON THE TITLE LINE ***";

/// A flattened tree: repository-relative path → (blob/tree-leaf id, entry kind).
type Flat = BTreeMap<BString, (ObjectId, EntryKind)>;

/// The `--quit`/`--continue`/`--abort`/`--skip` command modes. Exactly one may
/// be in effect; a second, different one is a usage error.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cmd {
    Quit,
    Continue,
    Abort,
    Skip,
}

impl Cmd {
    fn flag(self) -> &'static str {
        match self {
            Cmd::Quit => "--quit",
            Cmd::Continue => "--continue",
            Cmd::Abort => "--abort",
            Cmd::Skip => "--skip",
        }
    }
}

/// How `--cleanup` says the message should be tidied before committing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cleanup {
    Verbatim,
    Whitespace,
    Strip,
    Scissors,
    Default,
}

impl Cleanup {
    fn parse(arg: &str) -> Option<Cleanup> {
        Some(match arg {
            "verbatim" => Cleanup::Verbatim,
            "whitespace" => Cleanup::Whitespace,
            "strip" => Cleanup::Strip,
            "scissors" => Cleanup::Scissors,
            "default" => Cleanup::Default,
            _ => return None,
        })
    }
}

/// Everything the option parser collects, in git's own shape.
#[derive(Default)]
struct Options {
    no_commit: bool,
    signoff: bool,
    edit: bool,
    reference: bool,
    /// 0 means "not given"; git stores it the same way.
    mainline: usize,
    cleanup: Option<String>,
    /// Kept only so `--strategy-option` can be reported as incompatible with a
    /// command mode; a revert never consults the value.
    xopts: usize,
    /// `Some(true)` for `--rerere-autoupdate`, `Some(false)` for the negation.
    rerere: Option<bool>,
    mode: Option<Cmd>,
}

pub fn revert(args: &[String]) -> Result<ExitCode> {
    // `dispatch` hands over the operand list without the verb; tolerate a
    // leading literal `revert` so the module also works if it is ever wired
    // with the full argv.
    let args = match args.first() {
        Some(a) if a == "revert" => &args[1..],
        _ => args,
    };

    let mut o = Options::default();
    // git keeps unrecognized options in the operand list (`PARSE_OPT_KEEP_UNKNOWN_OPT`)
    // and only diagnoses them once the revision parser gets to them.
    let mut specs: Vec<String> = Vec::new();
    let mut no_more_opts = false;
    // git reads `revert.reference` in `git_revert_config` before parse_options,
    // so it is only the default: an explicit `--reference`/`--no-reference` on
    // the command line wins. Track whether either was seen so the config is
    // applied only when neither was.
    let mut reference_explicit = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if no_more_opts || !a.starts_with('-') || a == "-" {
            specs.push(a.to_string());
            i += 1;
            continue;
        }
        match a {
            "--" => no_more_opts = true,
            "-n" | "--no-commit" => o.no_commit = true,
            "--commit" => o.no_commit = false,
            "-s" | "--signoff" => o.signoff = true,
            "--no-signoff" => o.signoff = false,
            "-e" | "--edit" => o.edit = true,
            "--no-edit" => o.edit = false,
            "--reference" => {
                o.reference = true;
                reference_explicit = true;
            }
            "--no-reference" => {
                o.reference = false;
                reference_explicit = true;
            }
            "--rerere-autoupdate" => o.rerere = Some(true),
            "--no-rerere-autoupdate" => o.rerere = Some(false),
            "--no-gpg-sign" => {}
            "--no-mainline" => o.mainline = 0,
            "--no-cleanup" => o.cleanup = None,
            "--no-strategy" => {}
            "--no-strategy-option" => o.xopts = 0,
            "-m" | "--mainline" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("error: option `mainline' requires a value");
                    return Ok(ExitCode::from(129));
                };
                match parse_mainline(v) {
                    Some(n) => o.mainline = n,
                    None => return Ok(bad_mainline()),
                }
            }
            "--cleanup" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("error: option `cleanup' requires a value");
                    return Ok(ExitCode::from(129));
                };
                o.cleanup = Some(v.clone());
            }
            "--strategy" => {
                i += 1;
                if args.get(i).is_none() {
                    eprintln!("error: option `strategy' requires a value");
                    return Ok(ExitCode::from(129));
                }
            }
            "-X" | "--strategy-option" => {
                i += 1;
                if args.get(i).is_none() {
                    eprintln!("error: option `strategy-option' requires a value");
                    return Ok(ExitCode::from(129));
                }
                o.xopts += 1;
            }
            "--quit" => {
                if let Some(code) = set_mode(&mut o, Cmd::Quit) {
                    return Ok(code);
                }
            }
            "--continue" => {
                if let Some(code) = set_mode(&mut o, Cmd::Continue) {
                    return Ok(code);
                }
            }
            "--abort" => {
                if let Some(code) = set_mode(&mut o, Cmd::Abort) {
                    return Ok(code);
                }
            }
            "--skip" => {
                if let Some(code) = set_mode(&mut o, Cmd::Skip) {
                    return Ok(code);
                }
            }
            _ if a.starts_with("--mainline=") => match parse_mainline(&a["--mainline=".len()..]) {
                Some(n) => o.mainline = n,
                None => return Ok(bad_mainline()),
            },
            _ if a.starts_with("-m") && !a.starts_with("--") => match parse_mainline(&a[2..]) {
                Some(n) => o.mainline = n,
                None => return Ok(bad_mainline()),
            },
            _ if a.starts_with("--cleanup=") => {
                o.cleanup = Some(a["--cleanup=".len()..].to_string());
            }
            _ if a.starts_with("--strategy-option=") => o.xopts += 1,
            _ if a.starts_with("-X") && !a.starts_with("--") => o.xopts += 1,
            _ if a.starts_with("--strategy=") => {}
            _ if a.starts_with("-S") || a.starts_with("--gpg-sign") => {
                bail!("GPG signing is not supported")
            }
            // Unknown: git keeps it for the revision parser, which then fails
            // with the usage text. Mirror that by deferring the diagnosis.
            _ => specs.push(a.to_string()),
        }
        i += 1;
    }

    // Post-parse, in git's order: cleanup mode, then command-mode compatibility.
    let cleanup = match o.cleanup.as_deref() {
        None => None,
        Some(arg) => match Cleanup::parse(arg) {
            Some(c) => Some(c),
            None => {
                eprintln!("fatal: Invalid cleanup mode {arg}");
                return Ok(ExitCode::from(128));
            }
        },
    };

    let repo = gix::discover(".")?;
    if repo.workdir().is_none() {
        eprintln!("fatal: this operation must be run in a work tree");
        return Ok(ExitCode::from(128));
    }
    // `revert.reference` is the default for `--reference`; an explicit flag on
    // the command line already set `o.reference` and takes precedence.
    if !reference_explicit {
        if let Some(v) = repo.config_snapshot().boolean("revert.reference") {
            o.reference = v;
        }
    }
    // `--reference` implies editing, which is what makes `--cleanup=default`
    // behave as `strip` and drop the generated `#` title line.
    if o.reference {
        o.edit = true;
    }
    // Every step below mutates the index, the worktree and a ref: serialize the
    // whole sequence through the repo coordinator, as the other writers do.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    if let Some(mode) = o.mode {
        // git's `verify_opt_compatible` walks this list and reports the first
        // option that is set. `--strategy` is deliberately absent: git does not
        // check it here, so neither does this.
        for (name, active) in [
            ("--no-commit", o.no_commit),
            ("--signoff", o.signoff),
            ("--mainline", o.mainline != 0),
            ("--strategy-option", o.xopts > 0),
            ("--rerere-autoupdate", o.rerere == Some(true)),
            ("--no-rerere-autoupdate", o.rerere == Some(false)),
        ] {
            if active {
                eprintln!("fatal: revert: {name} cannot be used with {}", mode.flag());
                return Ok(ExitCode::from(128));
            }
        }
        if !specs.is_empty() {
            return Ok(usage_error());
        }
        return run_mode(&repo, mode);
    }

    if specs.is_empty() {
        return Ok(usage_error());
    }
    // Options git did not recognize reach the revision parser. It scans the
    // operand list left to right: a bad *revision* is diagnosed the moment it is
    // reached (`fatal: bad revision …`, exit 128), while an unrecognized dash
    // operand is only deferred and reported as the usage text (exit 129) after
    // the whole list is walked. So a bad revision outranks any dash token that
    // follows it — the diagnosis order is handled inside `resolve_specs`, not by
    // a blanket pre-scan here.

    let (commits, sequencer) = match resolve_specs(&repo, &specs)? {
        Selection::List { commits, sequencer } => (commits, sequencer),
        Selection::Failed(code) => return Ok(code),
    };
    if commits.is_empty() {
        eprintln!("error: empty commit set passed");
        eprintln!("fatal: revert failed");
        return Ok(ExitCode::from(128));
    }

    // With `-n` nothing is committed between steps; each further revert stacks
    // because it re-reads the index the previous one left behind.
    for id in commits {
        match revert_one(&repo, id, &o, cleanup, sequencer)? {
            Step::Failed(code) => return Ok(code),
            Step::Done => {}
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Record a command mode, rejecting a second and different one the way git's
/// `OPT_CMDMODE` does: the newly seen flag is named first.
fn set_mode(o: &mut Options, new: Cmd) -> Option<ExitCode> {
    match o.mode {
        Some(old) if old != new => {
            eprintln!(
                "error: options '{}' and '{}' cannot be used together",
                new.flag(),
                old.flag()
            );
            Some(ExitCode::from(129))
        }
        _ => {
            o.mode = Some(new);
            None
        }
    }
}

/// Run a `--quit`/`--continue`/`--abort`/`--skip` command mode.
///
/// Nothing here resumes a sequence: `--quit` drops the sequencer directory, and
/// the others report the "nothing in progress" refusal when there is no state,
/// which is the only branch this port can serve faithfully.
fn run_mode(repo: &gix::Repository, mode: Cmd) -> Result<ExitCode> {
    let git_dir = repo.git_dir();
    let sequencer = git_dir.join("sequencer");
    let revert_head = git_dir.join("REVERT_HEAD").exists();
    let cherry_pick_head = git_dir.join("CHERRY_PICK_HEAD").exists();

    match mode {
        Cmd::Quit => {
            let _ = std::fs::remove_dir_all(&sequencer);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Abort | Cmd::Continue => {
            if !sequencer.join("head").exists() && !revert_head && !cherry_pick_head {
                eprintln!("error: no cherry-pick or revert in progress");
                eprintln!("fatal: revert failed");
                return Ok(ExitCode::from(128));
            }
            bail!(
                "{} with a revert in progress is not supported (`git reset --merge` is not ported)",
                mode.flag()
            )
        }
        Cmd::Skip => {
            if !revert_head {
                eprintln!("error: no revert in progress");
                eprintln!("fatal: revert failed");
                return Ok(ExitCode::from(128));
            }
            bail!("--skip with a revert in progress is not supported (`git reset --merge` is not ported)")
        }
    }
}

/// Turn the operand list into the commits to revert, newest first.
///
/// A plain `<commit>` is taken as given and in order, which is what git's
/// `no_walk` sequencer setup produces. As soon as a range or a `^<commit>`
/// exclusion appears, the whole set goes through one revision walk instead.
///
/// `Failed` carries the exit code of a refusal whose text git has already
/// printed; `List` is the commit sequence to work through.
enum Selection {
    /// The commits to work through, plus whether git's sequencer is in play.
    /// A lone plain `<commit>` takes git's `no_walk` single-pick fast path,
    /// which never persists a sequencer or reports a revert "in progress"; any
    /// range/`^`-exclusion, or more than one operand, switches to the walking
    /// sequencer, which does. That flag is observable: when such a pick reverts
    /// to an empty result it stops with the `Revert currently in progress` block
    /// (`git revert HEAD..x` yields it even for a single walked commit, while
    /// `git revert x` alone does not).
    List { commits: Vec<ObjectId>, sequencer: bool },
    Failed(ExitCode),
}

fn resolve_specs(repo: &gix::Repository, specs: &[String]) -> Result<Selection> {
    let mut include: Vec<ObjectId> = Vec::new();
    let mut exclude: Vec<ObjectId> = Vec::new();
    let mut walked = false;
    let mut unknown_option = false;

    for spec in specs {
        // A dash-prefixed operand is not a revision: git's `setup_revisions`
        // keeps it as an unrecognized option and, unless a bad revision is hit
        // first, reports the usage error (129) only after the whole list is
        // scanned. A lone `-` is the exception — git rewrites it to `@{-1}`.
        if spec.starts_with('-') && spec != "-" {
            unknown_option = true;
            continue;
        }
        // git rewrites a lone `-` into the previously checked-out branch.
        let spec = if spec == "-" { "@{-1}" } else { spec.as_str() };

        if let Some((lhs, rhs)) = spec.split_once("..") {
            let lhs = if lhs.is_empty() { "HEAD" } else { lhs };
            let rhs = if rhs.is_empty() { "HEAD" } else { rhs };
            let (Some(from), Some(to)) = (peel_commit(repo, lhs), peel_commit(repo, rhs)) else {
                eprintln!("fatal: bad revision '{spec}'");
                return Ok(Selection::Failed(ExitCode::from(128)));
            };
            exclude.push(from);
            include.push(to);
            walked = true;
            continue;
        }

        let (negated, name) = match spec.strip_prefix('^') {
            Some(rest) => (true, rest),
            None => (false, spec),
        };
        let Some(id) = peel_commit(repo, name) else {
            eprintln!("fatal: bad revision '{spec}'");
            return Ok(Selection::Failed(ExitCode::from(128)));
        };
        if negated {
            exclude.push(id);
            walked = true;
        } else {
            include.push(id);
        }
    }

    // No bad revision was hit anywhere in the list; a deferred unrecognized dash
    // operand now surfaces as the usage error, exactly as git's post-scan option
    // check does — whether or not any valid commit was resolved.
    if unknown_option {
        return Ok(Selection::Failed(usage_error()));
    }

    if include.is_empty() || !walked {
        // No walk: git's single-pick fast path only when exactly one operand.
        let sequencer = include.len() > 1;
        return Ok(Selection::List { commits: include, sequencer });
    }
    let mut out = Vec::new();
    for info in repo.rev_walk(include).with_hidden(exclude).all()? {
        out.push(info?.id);
    }
    // A walked selection always runs through git's sequencer, even at length 1.
    Ok(Selection::List { commits: out, sequencer: true })
}

/// Resolve one revision to the commit it names, or `None` if either step fails
/// — git reports both as the same "bad revision".
fn peel_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let id = repo.rev_parse_single(spec).ok()?;
    Some(id.object().ok()?.peel_to_commit().ok()?.id)
}

/// Outcome of one `<commit>` operand.
enum Step {
    /// git reported a refusal itself (text already on stderr); stop with `code`.
    Failed(ExitCode),
    /// Applied.
    Done,
}

/// Revert a single commit, advancing `HEAD` unless `--no-commit` is set.
///
/// Under `--no-commit` the *ours* side is the current index written out as a
/// tree, exactly as git's `write_index_as_tree` does — so a pre-existing staged
/// change is merged through, and repeated `-n` steps stack on what the previous
/// one left staged. `Err` is reserved for the cases this port genuinely cannot
/// serve.
fn revert_one(
    repo: &gix::Repository,
    target_id: ObjectId,
    o: &Options,
    cleanup: Option<Cleanup>,
    sequencer: bool,
) -> Result<Step> {
    let target = repo.find_commit(target_id)?;
    let parents: Vec<ObjectId> = target.parent_ids().map(|id| id.detach()).collect();
    let is_merge = parents.len() > 1;

    let head = repo.head()?;
    if head.is_unborn() {
        eprintln!("fatal: Your current branch does not have any commits yet");
        return Ok(Step::Failed(ExitCode::from(128)));
    }
    let head_id = head
        .id()
        .ok_or_else(|| anyhow::anyhow!("HEAD does not point to a commit"))?
        .detach();
    drop(head);

    let hash = repo.object_hash();
    let head_tree = repo.find_commit(head_id)?.tree_id()?.detach();

    // git checks the index before it even looks at the commit's parents, so an
    // unmerged or dirty index outranks "is a merge but no -m was given". Under
    // `--no-commit` the only demand is that the index be merged, because the
    // index itself then becomes the *ours* side and a staged change is fine.
    let index_state = read_index_state(repo, head_tree)?;
    if o.no_commit {
        if index_state.unmerged {
            eprintln!("error: your index file is unmerged.");
            return Ok(Step::Failed(ExitCode::from(128)));
        }
    } else if index_state.unmerged {
        eprintln!("error: Reverting is not possible because you have unmerged files.");
        eprintln!("hint: Fix them up in the work tree, and then use 'git add/rm <file>'");
        eprintln!("hint: as appropriate to mark resolution and make a commit.");
        eprintln!("fatal: revert failed");
        return Ok(Step::Failed(ExitCode::from(128)));
    } else if index_state.differs_from_head {
        eprintln!("error: your local changes would be overwritten by revert.");
        eprintln!("hint: commit your changes or stash them to proceed.");
        eprintln!("fatal: revert failed");
        return Ok(Step::Failed(ExitCode::from(128)));
    }

    // *ours* is `HEAD`, or — under `--no-commit` — the index written out as a
    // tree. git does this here, before it even looks at the commit's parents, so
    // the tree object lands in the object database even on the refusals below.
    let ours_tree = if o.no_commit {
        match &index_state.staged {
            Some(staged) => write_tree(repo, staged)?,
            None => head_tree,
        }
    } else {
        head_tree
    };

    // Parent selection — git's rules, including `-m 1` being a silent no-op on a
    // non-merge commit and `-m N>1` there being an error.
    let parent_id: Option<ObjectId> = if is_merge {
        if o.mainline == 0 {
            eprintln!("error: commit {target_id} is a merge but no -m option was given.");
            eprintln!("fatal: revert failed");
            return Ok(Step::Failed(ExitCode::from(128)));
        }
        match parents.get(o.mainline - 1) {
            Some(p) => Some(*p),
            None => {
                eprintln!("error: commit {target_id} does not have parent {}", o.mainline);
                eprintln!("fatal: revert failed");
                return Ok(Step::Failed(ExitCode::from(128)));
            }
        }
    } else {
        if o.mainline > 1 {
            eprintln!("error: commit {target_id} does not have parent {}", o.mainline);
            eprintln!("fatal: revert failed");
            return Ok(Step::Failed(ExitCode::from(128)));
        }
        parents.first().copied()
    };

    let base_tree = target.tree_id()?.detach();
    // A root commit has no parent: reverting it means going back to nothing.
    let theirs_tree = match parent_id {
        Some(p) => repo.find_commit(p)?.tree_id()?.detach(),
        None => ObjectId::empty_tree(hash),
    };

    // --- the merge --------------------------------------------------------
    let base = flatten(repo, base_tree)?;
    let ours = flatten(repo, ours_tree)?;
    let theirs = flatten(repo, theirs_tree)?;
    let merged = merge_trivially(&base, &ours, &theirs, target_id)?;
    // The merge machinery writes its result tree before anything is checked out,
    // so the object exists even when the checkout below is refused.
    let merged_tree = write_tree(repo, &merged)?;

    // --- refuse to clobber ------------------------------------------------
    let changed: Vec<BString> = ours
        .keys()
        .chain(merged.keys())
        .filter(|p| ours.get(*p) != merged.get(*p))
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let wt = scan_worktree(repo)?;
    let mut clobbered: Vec<&BString> = changed
        .iter()
        .filter(|p| wt.modified.contains(*p))
        // A path the revert deletes that is already gone from the worktree has
        // nothing left to overwrite; git does not list it either.
        .filter(|p| {
            merged.contains_key(*p)
                || repo
                    .workdir_path(p.as_bstr())
                    .is_some_and(|full| full.exists())
        })
        .collect();
    if !clobbered.is_empty() {
        clobbered.sort();
        eprintln!("error: Your local changes to the following files would be overwritten by merge:");
        for p in clobbered {
            eprintln!("\t{}", quote_path(p));
        }
        eprintln!("Please commit your changes or stash them before you merge.");
        eprintln!("Aborting");
        eprintln!("fatal: revert failed");
        return Ok(Step::Failed(ExitCode::from(128)));
    }
    let mut untracked: Vec<&BString> = changed
        .iter()
        .filter(|p| !ours.contains_key(*p) && merged.contains_key(*p) && wt.untracked.contains(*p))
        .collect();
    if !untracked.is_empty() {
        untracked.sort();
        eprintln!("error: The following untracked working tree files would be overwritten by merge:");
        for p in untracked {
            eprintln!("\t{}", quote_path(p));
        }
        eprintln!("Please move or remove them before you merge.");
        eprintln!("Aborting");
        eprintln!("fatal: revert failed");
        return Ok(Step::Failed(ExitCode::from(128)));
    }
    // --- message ----------------------------------------------------------
    let committer = repo
        .committer()
        .ok_or_else(|| anyhow::anyhow!("no committer identity configured"))??;
    let message = build_message(
        repo,
        &target,
        parent_id.filter(|_| is_merge),
        o,
        cleanup,
        committer,
    )?;
    let subject = message.lines().next().unwrap_or("").to_string();

    // --- apply to index + worktree ---------------------------------------
    let changed_set: HashSet<BString> = changed.iter().cloned().collect();
    apply(repo, &changed_set, merged_tree, &merged)?;

    if o.no_commit {
        let git_dir = repo.git_dir();
        std::fs::write(git_dir.join("REVERT_HEAD"), format!("{target_id}\n"))?;
        std::fs::write(git_dir.join("MERGE_MSG"), &message)?;
        std::fs::write(git_dir.join("AUTO_MERGE"), format!("{merged_tree}\n"))?;
        return Ok(Step::Done);
    }

    // A revert that changes nothing produces no commit; git reports this via the
    // commit machinery and exits 1.
    if merged_tree == ours_tree {
        if !wt.modified.is_empty() || !wt.untracked.is_empty() {
            bail!("revert is a no-op and the `git status` advice for a dirty worktree is not ported");
        }
        match repo.head_name()? {
            Some(name) => println!("On branch {}", name.shorten()),
            None => println!("HEAD detached at {}", head_id.to_hex_with_len(7)),
        }
        // A sequencer pick that reverts to nothing stops mid-sequence rather
        // than ending, so `git commit`'s status carries the in-progress advice
        // git's `wt_status_get_state` prints from the live sequencer todo. The
        // single-pick fast path has no sequencer, so it omits this block.
        if sequencer {
            println!("Revert currently in progress.");
            println!("  (run \"git revert --continue\" to continue)");
            println!("  (use \"git revert --skip\" to skip this patch)");
            println!("  (use \"git revert --abort\" to cancel the revert operation)");
            println!();
        }
        println!("nothing to commit, working tree clean");
        return Ok(Step::Failed(ExitCode::from(1)));
    }

    // --- write the commit and move HEAD ----------------------------------
    let author = repo
        .author()
        .ok_or_else(|| anyhow::anyhow!("no author identity configured"))??;
    let author_time = author.time()?;
    let commit = gix::objs::Commit {
        message: message.clone().into(),
        tree: merged_tree,
        author: author.into(),
        committer: committer.into(),
        encoding: None,
        parents: std::iter::once(head_id).collect(),
        extra_headers: Default::default(),
    };
    let new_id = repo.write_object(&commit)?.detach();
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("revert: {subject}").into(),
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(head_id)),
            new: Target::Object(new_id),
        },
        name: "HEAD"
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
        deref: true,
    })?;

    // A committed revert clears any leftover in-progress markers, as git does.
    for f in ["REVERT_HEAD", "MERGE_MSG", "AUTO_MERGE"] {
        let _ = std::fs::remove_file(repo.git_dir().join(f));
    }

    print_summary(repo, new_id, &subject, &author_time, ours_tree, merged_tree)?;
    Ok(Step::Done)
}

/// `-m <n>` value parsing with `strtol` semantics: leading blanks are skipped,
/// trailing garbage is rejected, and only values above zero are accepted — git
/// reports all three failures with the same message.
fn parse_mainline(v: &str) -> Option<usize> {
    let t = v.trim_start();
    let t = t.strip_prefix('+').unwrap_or(t);
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<usize>().ok().filter(|n| *n > 0)
}

fn bad_mainline() -> ExitCode {
    eprintln!("error: option `mainline' expects a number greater than zero");
    ExitCode::from(129)
}

/// git's parse-options failure: full usage on stderr, exit 129.
fn usage_error() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Flatten a tree into `path -> (id, kind)` via the index representation, which
/// already expands nested trees into full slash-separated paths.
fn flatten(repo: &gix::Repository, tree: ObjectId) -> Result<Flat> {
    let index = repo.index_from_tree(&tree)?;
    let backing = index.path_backing();
    let mut out = Flat::new();
    for e in index.entries() {
        let path = e.path_in(backing);
        let mode: EntryMode = e
            .mode
            .to_tree_entry_mode()
            .ok_or_else(|| anyhow::anyhow!("tree entry `{path}` has an unrepresentable mode"))?;
        out.insert(path.to_owned(), (e.id, mode.kind()));
    }
    Ok(out)
}

/// What git checks about the index before it will start a revert.
struct IndexState {
    /// Any entry is at a conflict stage.
    unmerged: bool,
    /// The index does not match the `HEAD` tree.
    differs_from_head: bool,
    /// The index flattened the same way a tree is, so `--no-commit` can write it
    /// out as the *ours* side. `None` when there is no readable index at all.
    staged: Option<Flat>,
}

fn read_index_state(repo: &gix::Repository, head_tree: ObjectId) -> Result<IndexState> {
    let Ok(index) = repo.index() else {
        return Ok(IndexState {
            unmerged: false,
            differs_from_head: false,
            staged: None,
        });
    };
    let unmerged = index
        .entries()
        .iter()
        .any(|e| e.stage() != gix::index::entry::Stage::Unconflicted);
    if unmerged {
        return Ok(IndexState {
            unmerged: true,
            differs_from_head: true,
            staged: None,
        });
    }
    let backing = index.path_backing();
    let mut staged = Flat::new();
    for e in index.entries() {
        let path = e.path_in(backing);
        let Some(mode) = e.mode.to_tree_entry_mode() else {
            continue;
        };
        staged.insert(path.to_owned(), (e.id, mode.kind()));
    }
    let head = flatten(repo, head_tree)?;
    Ok(IndexState {
        unmerged: false,
        differs_from_head: staged != head,
        staged: Some(staged),
    })
}

/// Three-way merge restricted to the trivially resolvable cases.
///
/// For each path, with `b`/`o`/`t` the base/ours/theirs values: identical sides
/// resolve to that value, a side equal to the base yields the other side. A path
/// both sides moved off the base needs a content merge, which is not available
/// here (see the module note) and is refused instead of guessed.
fn merge_trivially(base: &Flat, ours: &Flat, theirs: &Flat, target: ObjectId) -> Result<Flat> {
    let mut paths: Vec<&BString> = base.keys().chain(ours.keys()).chain(theirs.keys()).collect();
    paths.sort();
    paths.dedup();

    let mut out = Flat::new();
    let mut conflicts: Vec<&BString> = Vec::new();
    for p in paths {
        let (b, o, t) = (base.get(p), ours.get(p), theirs.get(p));
        let resolved = if o == t {
            o
        } else if o == b {
            t
        } else if t == b {
            o
        } else {
            conflicts.push(p);
            continue;
        };
        if let Some(v) = resolved {
            out.insert(p.clone(), *v);
        }
    }
    if !conflicts.is_empty() {
        let list: Vec<String> = conflicts.iter().map(|p| p.to_string()).collect();
        bail!(
            "reverting {target} needs a content-level merge for {} (gix-merge is not linked in this build; only trivial three-way resolutions are ported)",
            list.join(", ")
        );
    }
    Ok(out)
}

/// Write a flattened tree back into the object database.
fn write_tree(repo: &gix::Repository, flat: &Flat) -> Result<ObjectId> {
    let mut editor =
        gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, repo.object_hash());
    for (path, (id, kind)) in flat {
        editor.upsert(path.split(|&b| b == b'/').map(|c| c.as_bstr()), *kind, *id)?;
    }
    Ok(editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?)
}

/// The parts of `git status` this command needs to decide whether it may write.
struct WorktreeState {
    /// Tracked paths whose worktree content differs from the index.
    modified: HashSet<BString>,
    /// Untracked worktree paths.
    untracked: HashSet<BString>,
}

fn scan_worktree(repo: &gix::Repository) -> Result<WorktreeState> {
    let mut state = WorktreeState {
        modified: HashSet::new(),
        untracked: HashSet::new(),
    };
    let patterns: Vec<BString> = Vec::new();
    for item in repo.status(gix::progress::Discard)?.into_iter(patterns)? {
        match item? {
            gix::status::Item::TreeIndex(_) => {}
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::EntryStatus;
                match iw {
                    Item::Modification { rela_path, status, .. } => match status {
                        // Unmerged paths are diagnosed from the index before the
                        // scan runs; reaching one here is not a reason to stop.
                        EntryStatus::Conflict { .. } => {
                            state.modified.insert(rela_path);
                        }
                        EntryStatus::IntentToAdd => {
                            state.modified.insert(rela_path);
                        }
                        EntryStatus::NeedsUpdate(_) => {}
                        EntryStatus::Change(_) => {
                            state.modified.insert(rela_path);
                        }
                    },
                    Item::DirectoryContents { entry, .. } => {
                        if matches!(entry.status, gix::dir::entry::Status::Untracked) {
                            state.untracked.insert(entry.rela_path);
                        }
                    }
                    Item::Rewrite { .. } => {}
                }
            }
        }
    }
    Ok(state)
}

/// Move the index and worktree onto `merged_tree`, touching only `changed`.
///
/// Unrelated index entries are carried over verbatim (with their stats), so a
/// locally modified file outside the revert's footprint stays exactly as it was
/// — matching git, which only refuses when an *affected* path is dirty.
fn apply(
    repo: &gix::Repository,
    changed: &HashSet<BString>,
    merged_tree: ObjectId,
    merged: &Flat,
) -> Result<()> {
    if changed.is_empty() {
        return Ok(());
    }
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    // Check out just the changed paths that exist in the merged tree.
    let mut subset = repo.index_from_tree(&merged_tree)?;
    subset.remove_entries(|_, path, _| !changed.contains(&path.to_owned()));

    // A revert that only deletes leaves nothing to check out. The checkout
    // itself must not be called then: it takes the path storage out of the
    // index, and `gix-index` asserts that an entry-less state has no storage
    // left — `remove_entries` drops entries but keeps the storage.
    let mut fresh: HashMap<BString, (ObjectId, Mode, Flags, Stat)> = HashMap::new();
    if !subset.entries().is_empty() {
        let should_interrupt = AtomicBool::new(false);
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
            &should_interrupt,
            opts,
        )?;

        // Fresh stats produced by that checkout, plus the entry shape to stage.
        let backing = subset.path_backing();
        for e in subset.entries() {
            fresh.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.flags, e.stat));
        }
    }

    // Delete worktree entries the revert removes. A gitlink leaves a populated
    // directory behind, which git reports and then leaves alone.
    for path in changed {
        if merged.contains_key(path) {
            continue;
        }
        let Some(full) = repo.workdir_path(path.as_bstr()) else {
            continue;
        };
        if full.is_dir() {
            if std::fs::remove_dir(&full).is_err() {
                eprintln!("warning: unable to rmdir '{path}': Directory not empty");
            }
        } else {
            let _ = std::fs::remove_file(full);
        }
    }

    // Restage: drop every changed path from the current index, then push back
    // the ones the merged tree still has. Untouched entries keep their stats.
    let mut index = repo.index_or_load_from_head()?.into_owned();
    index.remove_entries(|_, path, _| changed.contains(&path.to_owned()));
    for path in changed {
        if let Some((id, mode, flags, stat)) = fresh.get(path) {
            index.dangerously_push_entry(*stat, *id, *flags, *mode, path.as_bstr());
        }
    }
    index.sort_entries();
    index.remove_tree();
    index.write(Default::default())?;
    Ok(())
}

/// Build the revert commit message exactly as `sequencer_format_revert_message`
/// does, then apply the `--cleanup` mode the way the commit machinery would.
fn build_message(
    repo: &gix::Repository,
    target: &gix::Commit<'_>,
    merge_parent: Option<ObjectId>,
    o: &Options,
    cleanup: Option<Cleanup>,
    committer: gix::actor::SignatureRef<'_>,
) -> Result<String> {
    let raw = target.message_raw()?.to_string();
    let orig_subject = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();

    let mut msg = if o.reference {
        format!("{REFERENCE_TITLE}\n")
    } else {
        // Reverting a revert reads better as "Reapply"; the original subject
        // already carries the closing quote. git leaves an already-nested
        // `Revert "Revert "…""` alone rather than unwinding it.
        match orig_subject.strip_prefix("Revert \"") {
            Some(rest) if !rest.starts_with("Revert \"") => format!("Reapply \"{rest}\n"),
            _ => format!("Revert \"{orig_subject}\"\n"),
        }
    };
    msg.push_str("\nThis reverts commit ");
    msg.push_str(&refer_to(repo, target.id, o.reference)?);
    if let Some(p) = merge_parent {
        msg.push_str(", reversing\nchanges made to ");
        msg.push_str(&refer_to(repo, p, o.reference)?);
    }
    msg.push_str(".\n");
    if o.signoff {
        msg.push_str(&format!(
            "\nSigned-off-by: {} <{}>\n",
            committer.name, committer.email
        ));
    }

    // Without an explicit `--cleanup` the message git generates never needs
    // tidying, so it is left byte-for-byte as built.
    let Some(mode) = cleanup else {
        return Ok(msg);
    };
    let mode = match mode {
        Cleanup::Default => {
            if o.edit {
                Cleanup::Strip
            } else {
                Cleanup::Whitespace
            }
        }
        other => other,
    };
    Ok(match mode {
        Cleanup::Verbatim => msg,
        Cleanup::Strip => stripspace(&msg, true),
        // `scissors` only cuts at the scissors line, which this message never
        // has, so what remains is the plain whitespace tidy-up.
        Cleanup::Whitespace | Cleanup::Scissors => stripspace(&msg, false),
        Cleanup::Default => unreachable!("resolved above"),
    })
}

/// How git refers to a commit in the message: the full hex id normally, and
/// `%h (%s, %ad)` with a short date under `--reference`.
fn refer_to(repo: &gix::Repository, id: ObjectId, reference: bool) -> Result<String> {
    if !reference {
        return Ok(id.to_string());
    }
    let commit = repo.find_commit(id)?;
    let short = id.attach(repo).shorten_or_id();
    let raw = commit.message_raw()?.to_string();
    let subject = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();
    let date = commit
        .author()?
        .time()?
        .format_or_unix(gix::date::time::format::SHORT);
    Ok(format!("{short} ({subject}, {date})"))
}

/// git's `strbuf_stripspace`: drop trailing whitespace, collapse runs of blank
/// lines to one, remove leading and trailing blanks, and — when asked — drop
/// whole comment lines.
fn stripspace(s: &str, strip_comments: bool) -> String {
    let mut out = String::new();
    let mut wrote = false;
    let mut pending_blank = false;
    for line in s.lines() {
        if strip_comments && line.starts_with('#') {
            continue;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            pending_blank = wrote;
            continue;
        }
        if pending_blank {
            out.push('\n');
            pending_blank = false;
        }
        out.push_str(trimmed);
        out.push('\n');
        wrote = true;
    }
    out
}

/// The block git prints after a successful revert: the id/subject line, the
/// author `Date:` line the sequencer always requests, the short-stat, and the
/// create/delete/mode-change summary.
fn print_summary(
    repo: &gix::Repository,
    new_id: ObjectId,
    subject: &str,
    author_time: &gix::date::Time,
    old_tree: ObjectId,
    new_tree: ObjectId,
) -> Result<()> {
    let label = match repo.head_name()? {
        Some(name) => name.shorten().to_string(),
        None => "detached HEAD".to_string(),
    };
    let short = new_id.attach(repo).shorten_or_id();
    println!("[{label} {short}] {subject}");
    println!(
        " Date: {}",
        author_time.format_or_unix(gix::date::time::format::DEFAULT)
    );

    let old = flatten(repo, old_tree)?;
    let new = flatten(repo, new_tree)?;

    let mut files_changed: u64 = 0;
    let mut summary: Vec<(BString, String)> = Vec::new();
    // A gitlink is one line of diff (`Subproject commit <oid>`) that the blob
    // differ below cannot see at all, so it is counted here.
    let (mut link_ins, mut link_del) = (0u64, 0u64);
    for (path, (id, kind)) in &new {
        match old.get(path) {
            None => {
                files_changed += 1;
                if *kind == EntryKind::Commit {
                    link_ins += 1;
                }
                summary.push((path.clone(), format!("create mode {} {}", octal(*kind), quote_path(path))));
            }
            Some((old_id, old_kind)) => {
                if old_id != id || old_kind != kind {
                    files_changed += 1;
                    if *kind == EntryKind::Commit {
                        link_ins += 1;
                    }
                    if *old_kind == EntryKind::Commit {
                        link_del += 1;
                    }
                }
                if old_kind != kind {
                    summary.push((
                        path.clone(),
                        format!(
                            "mode change {} => {} {}",
                            octal(*old_kind),
                            octal(*kind),
                            quote_path(path)
                        ),
                    ));
                }
            }
        }
    }
    for (path, (_, kind)) in &old {
        if !new.contains_key(path) {
            files_changed += 1;
            if *kind == EntryKind::Commit {
                link_del += 1;
            }
            summary.push((path.clone(), format!("delete mode {} {}", octal(*kind), quote_path(path))));
        }
    }

    // Line counts from a real blob diff; rename detection is off so the file
    // accounting stays consistent with the counts above.
    let old_obj = repo.find_tree(old_tree)?;
    let new_obj = repo.find_tree(new_tree)?;
    let mut platform = old_obj.changes()?;
    platform.options(|o| {
        o.track_rewrites(None);
    });
    let stats = platform.stats(&new_obj)?;

    if files_changed > 0 {
        let ins = stats.lines_added + link_ins;
        let del = stats.lines_removed + link_del;
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
    }
    Ok(())
}

/// git's `quote_c_style`, which every path in the summary block goes through.
///
/// A path is left alone unless it holds a byte that needs escaping; then the
/// whole thing is wrapped in double quotes with C escapes. With `core.quotePath`
/// at its default, that includes every byte above ASCII, so `üñïçødé.txt` prints
/// as `"\303\274\303\261\303\257\303\247\303\270d\303\251.txt"` — while a plain
/// space, which needs no escape, keeps the path unquoted.
fn quote_path(path: &BString) -> String {
    let needs_quoting = path
        .iter()
        .any(|&b| b < 0x20 || b >= 0x7f || b == b'"' || b == b'\\');
    if !needs_quoting {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for &b in path.iter() {
        match b {
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            _ if b < 0x20 || b >= 0x7f => out.push_str(&format!("\\{b:03o}")),
            _ => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// The 6-digit octal mode git prints in create/delete/mode-change lines.
fn octal(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Tree => "040000",
        EntryKind::Blob => "100644",
        EntryKind::BlobExecutable => "100755",
        EntryKind::Link => "120000",
        EntryKind::Commit => "160000",
    }
}

/// `""` for a count of 1, `"s"` otherwise.
fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
