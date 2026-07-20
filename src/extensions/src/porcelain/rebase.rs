//! `git rebase` — reapply commits on top of another base tip.
//!
//! ### What is ported
//!
//! Only the two branches of `cmd_rebase()` that do **not** replay any commit are
//! served natively, plus the argument handling and the pre-flight checks that
//! guard them. For these, stdout, stderr, the exit code, the refs, the reflogs,
//! `ORIG_HEAD`, the index and the worktree all match stock git:
//!
//! * **up to date** — when `can_fast_forward()` holds (see [`is_up_to_date`]),
//!   git prints `Current branch <name> is up to date.` (or `HEAD is up to date.`
//!   on a detached `HEAD`) on **stdout**, exits 0 and touches nothing at all —
//!   not even `ORIG_HEAD`.
//! * **empty todo list** — when `<upstream>..<head>` contains no commit, the
//!   sequencer has nothing to pick and simply moves the branch to `<onto>`.
//!   `ORIG_HEAD` is written, `HEAD` is detached at `<onto>` with a
//!   `rebase (start): checkout <onto-spec>` reflog entry, the branch is updated
//!   with `rebase (finish): <ref> onto <onto-oid>`, `HEAD` is re-attached with
//!   `rebase (finish): returning to <ref>`, and
//!   `Successfully rebased and updated <ref>.` goes to **stderr**. On a detached
//!   `HEAD` only the `rebase (start)` entry is written and the message names
//!   `detached HEAD`, exactly as git does.
//! * the `require_clean_work_tree()` refusal, byte for byte including the
//!   `additionally, your index contains uncommitted changes.` second line.
//! * `fatal: invalid upstream '<spec>'`, `fatal: Does not point to a valid
//!   commit '<spec>'` and `fatal: no rebase in progress` (all exit 128), the
//!   missing-tracking-information block (exit 1, on stdout), and `-h` / unknown
//!   option usage (exit 129).
//!
//! ### What is NOT ported, and why
//!
//! Any invocation that would have to replay a commit bails. Two *independent*
//! blockers stand in the way, and neither can be worked around from a single
//! file:
//!
//! 1. **No three-way tree merge.** A pick is a merge of the picked commit's tree
//!    against `HEAD`'s over the commit's first parent. `gix::merge`,
//!    `Repository::merge_trees()` and `Repository::merge_commits()` all sit
//!    behind the `gix` crate's `merge` feature (`gix/Cargo.toml`: `merge =
//!    ["tree-editor", "blob-diff", "dep:gix-merge", "attributes"]`), which is
//!    listed under `need-more-recent-msrv` and so is *not* in `default`;
//!    `src/extensions/Cargo.toml` enables only
//!    `blocking-http-transport-reqwest-rust-tls` and `tree-editor`. The vendored
//!    `gix-rebase` crate is an empty placeholder (`#![forbid(unsafe_code)]` and
//!    nothing else).
//! 2. **No patch-id equivalence.** Default `git rebase` (i.e. without
//!    `--reapply-cherry-picks`) *drops* every to-be-rebased commit whose patch
//!    is already present in `<upstream>`, printing `warning: skipped previously
//!    applied commit <abbrev>` plus two `hint:` lines. Deciding that needs a
//!    patch-id per commit on both sides of the symmetric difference. Nothing in
//!    the vendored crates computes one — `porcelain::patch_id` is a stdin filter
//!    over pre-rendered diff text and exposes no reusable entry point. Getting
//!    this wrong does not merely lose output fidelity: it silently *keeps*
//!    commits git would have dropped, duplicating history while appearing to
//!    succeed.
//!
//! Blocker 2 stands even where blocker 1 could be side-stepped with the
//! file-granularity resolution `porcelain::cherry_pick` uses, because a real
//! `git rebase <upstream>` almost always has upstream-only commits to compare
//! against. So the replay path is refused outright rather than approximated.
//!
//! The same applies transitively to everything built on replay: `--continue`,
//! `--skip`, `--abort`, `--quit`, `--edit-todo`, `--show-current-patch` (which
//! additionally need the `.git/rebase-merge` state directory this port never
//! creates), `-i`, `--exec`, `--autosquash`, `--rebase-merges`, `--root`,
//! `--keep-base`, `--fork-point`, `--autostash`, `--signoff`, `-s`/`-X`,
//! `--empty`, `--update-refs` and the patch-id de-duplication that default
//! `git rebase` performs (`warning: skipped previously applied commit …`).
//! Every one of them is rejected with a precise message; none is silently
//! ignored.
//!
//! Ranges containing a merge commit are refused as well: git flattens them by
//! dropping the merges and replaying the rest in `--topo-order`, and neither the
//! replay nor that ordering is reproduced here.

use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use gix::bstr::BString;
use gix::hash::ObjectId;
use gix::index::entry::{Mode, Stat};
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use gix::refs::{FullName, Target};

/// git's `builtin/rebase.c` usage block, reproduced verbatim (git 2.55.0) so the
/// `-h` and unknown-option paths are byte-identical.
const USAGE: &str = "\
usage: git rebase [-i] [options] [--exec <cmd>] [--onto <newbase> | --keep-base] [<upstream> [<branch>]]
   or: git rebase [-i] [options] [--exec <cmd>] [--onto <newbase>] --root [<branch>]
   or: git rebase --continue | --abort | --skip | --edit-todo

    --[no-]onto <revision>
                          rebase onto given branch instead of upstream
    --[no-]keep-base      use the merge-base of upstream and branch as the current base
    --no-verify           allow pre-rebase hook to run
    --verify              opposite of --no-verify
    -q, --[no-]quiet      be quiet. implies --no-stat
    -v, --[no-]verbose    display a diffstat of what changed upstream
    -n, --no-stat         do not show diffstat of what changed upstream
    --stat                opposite of --no-stat
    --[no-]trailer <trailer>
                          add custom trailer(s)
    --[no-]signoff        add a Signed-off-by trailer to each commit
    --[no-]committer-date-is-author-date
                          make committer date match author date
    --[no-]reset-author-date
                          ignore author date and use current date
    -C <n>                passed to 'git apply'
    --[no-]ignore-whitespace
                          ignore changes in whitespace
    --[no-]whitespace <action>
                          passed to 'git apply'
    -f, --[no-]force-rebase
                          cherry-pick all commits, even if unchanged
    --no-ff               cherry-pick all commits, even if unchanged
    --ff                  opposite of --no-ff
    --continue            continue
    --skip                skip current patch and continue
    --abort               abort and check out the original branch
    --quit                abort but keep HEAD where it is
    --edit-todo           edit the todo list during an interactive rebase
    --show-current-patch  show the patch file being applied or merged
    --apply               use apply strategies to rebase
    -m, --merge           use merging strategies to rebase
    -i, --interactive     let the user edit the list of commits to rebase
    --[no-]rerere-autoupdate
                          update the index with reused conflict resolution if possible
    --empty (drop|keep|stop)
                          how to handle commits that become empty
    --[no-]autosquash     move commits that begin with squash!/fixup! under -i
    --[no-]update-refs    update branches that point to commits that are being rebased
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG-sign commits
    --[no-]autostash      automatically stash/stash pop before and after
    -x, --[no-]exec <exec>
                          add exec lines after each commit of the editable list
    -r, --[no-]rebase-merges[=<mode>]
                          try to rebase merges instead of skipping them
    --[no-]fork-point     use 'merge-base --fork-point' to refine upstream
    -s, --[no-]strategy <strategy>
                          use the given merge strategy
    -X, --[no-]strategy-option <option>
                          pass the argument through to the merge strategy
    --[no-]root           rebase all reachable commits up to the root(s)
    --[no-]reschedule-failed-exec
                          automatically re-schedule any `exec` that fails
    --[no-]reapply-cherry-picks
                          apply all changes, even those already present upstream
";

/// The mode options of `git rebase`, which replace the normal invocation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ModeOption {
    Continue,
    Skip,
    Abort,
    Quit,
    EditTodo,
    ShowCurrentPatch,
}

impl ModeOption {
    fn flag(self) -> &'static str {
        match self {
            ModeOption::Continue => "--continue",
            ModeOption::Skip => "--skip",
            ModeOption::Abort => "--abort",
            ModeOption::Quit => "--quit",
            ModeOption::EditTodo => "--edit-todo",
            ModeOption::ShowCurrentPatch => "--show-current-patch",
        }
    }
}

pub fn rebase(args: &[String]) -> Result<ExitCode> {
    // --- argument parsing -------------------------------------------------
    // `args` excludes the subcommand: `lib.rs` splits `argv` into `sub` and
    // `rest = &argv[2..]`, and `dispatch::run` hands `rest` straight to the
    // porcelain fn (see `"commit" => porcelain::commit(args)` in dispatch.rs).
    let mut quiet = false;
    let mut onto_spec: Option<String> = None;
    let mut mode: Option<ModeOption> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "-q" | "--quiet" => quiet = true,
            "--no-quiet" => quiet = false,
            "--onto" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    // git's `parse_options` names the option without its dashes.
                    eprint!("error: option `onto' requires a value\n{USAGE}");
                    return Ok(ExitCode::from(129));
                };
                onto_spec = Some(v.clone());
            }
            "--continue" => mode = Some(ModeOption::Continue),
            "--skip" => mode = Some(ModeOption::Skip),
            "--abort" => mode = Some(ModeOption::Abort),
            "--quit" => mode = Some(ModeOption::Quit),
            "--edit-todo" => mode = Some(ModeOption::EditTodo),
            "--show-current-patch" => mode = Some(ModeOption::ShowCurrentPatch),

            // Recognized by stock git, but each one changes what history the
            // rebase produces, and none of them can be honoured without the
            // cherry-pick substrate (see the module note). Refuse explicitly.
            "-i" | "--interactive" => bail!("unsupported flag {a:?} (interactive rebase needs a todo list and commit replay; ported: --onto, -q/--quiet, <upstream>, <branch>)"),
            "-r" | "--rebase-merges" | "--no-rebase-merges" => bail!("unsupported flag {a:?} (merge replay is not ported)"),
            "--root" | "--no-root" => bail!("unsupported flag {a:?} (root rebase requires commit replay)"),
            "--keep-base" | "--no-keep-base" => bail!("unsupported flag {a:?} (keep-base requires commit replay)"),
            "--fork-point" | "--no-fork-point" => bail!("unsupported flag {a:?} (fork-point upstream refinement is not ported)"),
            "--autostash" | "--no-autostash" => bail!("unsupported flag {a:?} (autostash is not ported)"),
            "--autosquash" | "--no-autosquash" => bail!("unsupported flag {a:?} (autosquash requires an interactive todo list)"),
            "--update-refs" | "--no-update-refs" => bail!("unsupported flag {a:?} (update-refs requires commit replay)"),
            "--reapply-cherry-picks" | "--no-reapply-cherry-picks" => bail!("unsupported flag {a:?} (patch-id de-duplication is not ported)"),
            "--signoff" | "--no-signoff" => bail!("unsupported flag {a:?} (rewriting commit messages requires commit replay)"),
            "--committer-date-is-author-date" | "--no-committer-date-is-author-date" | "--reset-author-date" | "--no-reset-author-date" => {
                bail!("unsupported flag {a:?} (rewriting commit dates requires commit replay)")
            }
            "-f" | "--force-rebase" | "--no-ff" => bail!("unsupported flag {a:?} (forced replay of unchanged commits is not ported)"),
            "--ff" => bail!("unsupported flag {a:?} (only the no-replay fast-forward path is ported; drop the flag)"),
            "-m" | "--merge" | "--apply" => bail!("unsupported flag {a:?} (rebase backends are not ported)"),
            "-x" | "--exec" | "--no-exec" => bail!("unsupported flag {a:?} (exec lines require an interactive todo list)"),
            "-s" | "--strategy" | "-X" | "--strategy-option" => bail!("unsupported flag {a:?} (merge strategies are not ported)"),
            "-S" | "--gpg-sign" | "--no-gpg-sign" => bail!("unsupported flag {a:?} (signing requires commit replay)"),
            "--empty" => bail!("unsupported flag {a:?} (empty-commit handling requires commit replay)"),
            "-v" | "--verbose" | "--stat" | "--no-stat" | "-n" => bail!("unsupported flag {a:?} (the upstream diffstat is not ported)"),
            "--rerere-autoupdate" | "--no-rerere-autoupdate" => bail!("unsupported flag {a:?} (rerere is not ported)"),
            "--reschedule-failed-exec" | "--no-reschedule-failed-exec" => bail!("unsupported flag {a:?} (exec lines are not ported)"),
            "--verify" | "--no-verify" => bail!("unsupported flag {a:?} (the pre-rebase hook is not run)"),
            "--ignore-whitespace" | "--no-ignore-whitespace" | "--whitespace" | "-C" => {
                bail!("unsupported flag {a:?} (apply-backend options are not ported)")
            }
            "--trailer" | "--no-trailer" => bail!("unsupported flag {a:?} (trailers require commit replay)"),

            "--" => {
                if i + 1 < args.len() {
                    bail!("pathspec arguments are not accepted by rebase");
                }
            }
            s if s.starts_with("--onto=") => onto_spec = Some(s["--onto=".len()..].to_string()),
            s if s.starts_with("--empty=") => {
                bail!("unsupported flag {s:?} (empty-commit handling requires commit replay)")
            }
            s if s.starts_with("--strategy=") || s.starts_with("--strategy-option=") => {
                bail!("unsupported flag {s:?} (merge strategies are not ported)")
            }
            s if s.starts_with("--exec=") => {
                bail!("unsupported flag {s:?} (exec lines require an interactive todo list)")
            }
            // git's `parse_options` reports the long form without its dashes and
            // the short form as a "switch"; both then dump the usage block.
            s if s.starts_with("--") => {
                eprint!("error: unknown option `{}'\n{USAGE}", &s[2..]);
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with('-') && s.len() > 1 => {
                eprint!(
                    "error: unknown switch `{}'\n{USAGE}",
                    s.chars().nth(1).unwrap_or('?')
                );
                return Ok(ExitCode::from(129));
            }
            s => positional.push(s.to_string()),
        }
        i += 1;
    }

    if positional.len() > 2 {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    let repo = gix::discover(".")?;

    // --- mode options -----------------------------------------------------
    // Without a rebase in progress git dies the same way for every mode option,
    // and that is reproducible here exactly. With one in progress the state
    // directory would have to be understood and the picks resumed, which this
    // port cannot do.
    if let Some(m) = mode {
        if in_progress(&repo) {
            bail!(
                "unsupported flag {:?} (resuming a rebase requires replaying commits)",
                m.flag()
            );
        }
        eprintln!("fatal: no rebase in progress");
        return Ok(ExitCode::from(128));
    }

    // --- HEAD -------------------------------------------------------------
    let head = repo.head()?;
    if head.is_unborn() {
        bail!("cannot rebase an unborn branch");
    }
    let head_oid = head
        .id()
        .ok_or_else(|| anyhow!("HEAD does not point to a commit"))?
        .detach();
    // Owned branch name when attached, `None` when detached.
    let branch: Option<FullName> = head.referent_name().map(std::borrow::ToOwned::to_owned);
    drop(head);

    // git switches to `<branch>` before rebasing; that is a checkout, which is
    // out of scope here, so only the already-current branch is accepted.
    if let Some(requested) = positional.get(1) {
        let current = branch.as_ref().map(|b| b.shorten().to_string());
        if current.as_deref() != Some(requested.as_str()) {
            bail!(
                "rebasing a branch other than the checked-out one requires a checkout; run `git switch {requested}` first"
            );
        }
    }

    // --- <upstream> -------------------------------------------------------
    let upstream_spec = match positional.first() {
        Some(s) => s.clone(),
        None => {
            // git falls back to `branch.<name>.merge` / `branch.<name>.remote`.
            let tracking = branch.as_ref().and_then(|b| {
                repo.branch_remote_tracking_ref_name(b.as_ref(), gix::remote::Direction::Fetch)
            });
            match tracking {
                Some(Ok(name)) => name.shorten().to_string(),
                Some(Err(e)) => bail!("{e}"),
                None => {
                    let Some(b) = branch.as_ref() else {
                        bail!("HEAD is detached and no <upstream> was given");
                    };
                    // `error_on_missing_default_upstream()`: stdout, exit 1.
                    print!(
                        "There is no tracking information for the current branch.\n\
                         Please specify which branch you want to rebase against.\n\
                         See git-rebase(1) for details.\n\
                         \n    git rebase '<branch>'\n\n\
                         If you wish to set tracking information for this branch you can do so with:\n\
                         \n    git branch --set-upstream-to=<remote>/<branch> {}\n\n",
                        b.shorten().to_string()
                    );
                    return Ok(ExitCode::from(1));
                }
            }
        }
    };
    let Some(upstream_oid) = peel_to_commit(&repo, &upstream_spec) else {
        eprintln!("fatal: invalid upstream '{upstream_spec}'");
        return Ok(ExitCode::from(128));
    };

    // --- <onto> -----------------------------------------------------------
    let onto_spec = onto_spec.unwrap_or_else(|| upstream_spec.clone());
    let Some(onto_oid) = peel_to_commit(&repo, &onto_spec) else {
        eprintln!("fatal: Does not point to a valid commit '{onto_spec}'");
        return Ok(ExitCode::from(128));
    };

    // --- require_clean_work_tree() ---------------------------------------
    // Checked before anything is decided, and before `ORIG_HEAD` is written.
    let (unstaged, staged) = dirty_state(&repo)?;
    if unstaged || staged {
        if unstaged {
            eprintln!("error: cannot rebase: You have unstaged changes.");
            if staged {
                eprintln!("error: additionally, your index contains uncommitted changes.");
            }
        } else {
            eprintln!("error: cannot rebase: Your index contains uncommitted changes.");
        }
        eprintln!("error: Please commit or stash them.");
        return Ok(ExitCode::from(1));
    }

    // --- can_fast_forward(): nothing to do at all ------------------------
    if is_up_to_date(&repo, onto_oid, upstream_oid, head_oid)? {
        if !quiet {
            match branch.as_ref() {
                Some(b) => println!("Current branch {} is up to date.", b.shorten().to_string()),
                None => println!("HEAD is up to date."),
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    // --- the todo list ----------------------------------------------------
    // `<upstream>..<head>`, which is what the sequencer would pick.
    let mut todo: Vec<ObjectId> = Vec::new();
    for info in repo.rev_walk([head_oid]).with_hidden([upstream_oid]).all()? {
        todo.push(info?.id);
    }

    for id in &todo {
        if repo.find_commit(*id)?.parent_ids().count() > 1 {
            bail!("the range {upstream_spec}..HEAD contains merge commits; flattening them requires commit replay");
        }
    }

    if !todo.is_empty() {
        bail!(
            "replaying {} commit(s) needs a three-way tree merge (this build of `gix` has the \
             `merge` feature off) and patch-id equivalence to drop already-upstream commits \
             (unavailable); only the up-to-date and no-replay fast-forward paths are ported",
            todo.len()
        );
    }

    // --- no-replay path: move the branch to <onto> ------------------------
    // Serialize the whole read-modify-write through the repo coordinator (a
    // no-op when no daemon is running), matching the merge/zsync write path.
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    // Capture the current (clean) index BEFORE any ref moves: it mirrors the old
    // tree and carries the filesystem stats reused for unchanged files. Taken
    // first because `index_or_load_from_head` would otherwise fall back to the
    // *new* HEAD if a repository happened to have no index file on disk.
    let old_index = repo.index_or_load_from_head()?.into_owned();

    // git writes ORIG_HEAD only once it commits to actually rebasing. It is a
    // pseudo-ref, so no reflog is created for it (gix applies git's own
    // `should_autocreate_reflog` rule).
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "rebase".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(head_oid),
        },
        name: full_name("ORIG_HEAD")?,
        deref: false,
    })?;

    // The sequencer detaches HEAD at <onto> first, ...
    set_head(
        &repo,
        Target::Object(onto_oid),
        &format!("rebase (start): checkout {onto_spec}"),
    )?;

    // ... moves the worktree and index onto the new tree, ...
    let should_interrupt = AtomicBool::new(false);
    update_clean_worktree(&repo, &old_index, onto_oid, &should_interrupt)?;

    // ... then re-points the branch and re-attaches HEAD to it. On a detached
    // HEAD there is no branch, and git writes no `rebase (finish)` entry.
    let label = match &branch {
        Some(b) => {
            let name = b.as_bstr().to_string();
            repo.edit_reference(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: format!("rebase (finish): {name} onto {onto_oid}").into(),
                    },
                    expected: PreviousValue::MustExistAndMatch(Target::Object(head_oid)),
                    new: Target::Object(onto_oid),
                },
                name: b.clone(),
                deref: false,
            })?;
            set_head(
                &repo,
                Target::Symbolic(b.clone()),
                &format!("rebase (finish): returning to {name}"),
            )?;
            name
        }
        None => "detached HEAD".to_string(),
    };

    if !quiet {
        eprintln!("Successfully rebased and updated {label}.");
    }
    Ok(ExitCode::SUCCESS)
}

/// git's `can_fast_forward()`: true when `<head>` already sits on top of
/// `<onto>` and nothing between `<upstream>` and `<head>` would be replayed —
/// i.e. `merge-base(onto, head) == onto` and `merge-base(upstream, head) ==
/// onto`. Multiple merge-bases on either side make git give up on the shortcut,
/// so they do here too.
fn is_up_to_date(
    repo: &gix::Repository,
    onto: ObjectId,
    upstream: ObjectId,
    head: ObjectId,
) -> Result<bool> {
    let bases = repo.merge_bases_many(onto, &[head])?;
    if bases.len() != 1 || bases[0].detach() != onto {
        return Ok(false);
    }
    let bases = repo.merge_bases_many(upstream, &[head])?;
    Ok(bases.len() == 1 && bases[0].detach() == onto)
}

/// Resolve `spec` and peel it to a commit id, or `None` when either step fails —
/// git reports both as one "invalid" message rather than surfacing the cause.
fn peel_to_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let id = repo.rev_parse_single(spec).ok()?;
    Some(id.object().ok()?.peel_to_commit().ok()?.id)
}

/// `(worktree differs from index, index differs from HEAD)`, the two predicates
/// behind `has_unstaged_changes()` and `has_uncommitted_changes()`.
fn dirty_state(repo: &gix::Repository) -> Result<(bool, bool)> {
    let mut unstaged = false;
    let mut staged = false;
    let patterns: Vec<BString> = Vec::new();
    for item in repo.status(gix::progress::Discard)?.into_iter(patterns)? {
        match item? {
            gix::status::Item::TreeIndex(_) => staged = true,
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::EntryStatus;
                match iw {
                    Item::Modification { status, .. } => match status {
                        // Untracked and up-to-date entries do not block a rebase.
                        EntryStatus::NeedsUpdate(_) => {}
                        _ => unstaged = true,
                    },
                    Item::Rewrite { .. } => unstaged = true,
                    Item::DirectoryContents { .. } => {}
                }
            }
        }
    }
    Ok((unstaged, staged))
}

/// True when a rebase state directory is present, i.e. `git rebase --continue`
/// and friends would have something to act on.
fn in_progress(repo: &gix::Repository) -> bool {
    let dir = repo.common_dir();
    dir.join("rebase-merge").is_dir() || dir.join("rebase-apply").is_dir()
}

fn full_name(name: &str) -> Result<FullName> {
    name.try_into()
        .map_err(|e| anyhow!("invalid ref name {name}: {e}"))
}

/// Point `HEAD` at `target` (an object for a detached `HEAD`, a ref to attach
/// it), writing `message` to the `HEAD` reflog.
fn set_head(repo: &gix::Repository, target: Target, message: &str) -> Result<()> {
    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected: PreviousValue::Any,
            new: target,
        },
        name: full_name("HEAD")?,
        deref: false,
    })?;
    Ok(())
}

/// Move a clean worktree and its index from the state captured in `old` to the
/// tree of commit `new_commit`, writing only the files that changed.
///
/// Same reconcile path as `porcelain::merge`: the change set is derived by
/// comparing the old index against the new tree-index (file-level granularity),
/// added/modified files are checked out via `gix-worktree-state`, removed files
/// are deleted, and the new index is written reusing prior stats for unchanged
/// entries so a later status stays cheap.
fn update_clean_worktree(
    repo: &gix::Repository,
    old: &gix::index::File,
    new_commit: ObjectId,
    should_interrupt: &AtomicBool,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("bare repository has no worktree to update"))?
        .to_owned();

    let new_tree_id = repo.find_object(new_commit)?.peel_to_tree()?.id;

    // Index the current entries by path for change detection and stat reuse.
    let mut old_map: HashMap<BString, (ObjectId, Mode, Stat)> =
        HashMap::with_capacity(old.entries().len());
    {
        let backing = old.path_backing();
        for e in old.entries() {
            old_map.insert(e.path_in(backing).to_owned(), (e.id, e.mode, e.stat));
        }
    }

    // Full target index (all new-tree entries) — what is finally written; a
    // reduced copy of only the changed entries is what is checked out.
    let mut new_index = repo.index_from_tree(&new_tree_id)?;
    let mut subset = repo.index_from_tree(&new_tree_id)?;
    subset.remove_entries(|_, path, entry| match old_map.get(&path.to_owned()) {
        // Present before with identical content and mode → unchanged, drop it.
        Some((oid, mode, _)) => *oid == entry.id && *mode == entry.mode,
        // Absent before → an addition, keep it.
        None => false,
    });

    // Write the changed files into the (clean) worktree, overwriting in place.
    let mut opts =
        repo.checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)?;
    opts.destination_is_initially_empty = false;
    opts.overwrite_existing = true;
    let odb = repo.objects.clone().into_arc()?;
    gix::worktree::state::checkout(
        &mut subset,
        workdir.as_path(),
        odb,
        &gix::progress::Discard,
        &gix::progress::Discard,
        should_interrupt,
        opts,
    )?;

    // Remove files present in the old index but not the new tree.
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

    // Fresh stats produced by the checkout for the changed entries.
    let mut subset_stats: HashMap<BString, Stat> = HashMap::with_capacity(subset.entries().len());
    {
        let backing = subset.path_backing();
        for e in subset.entries() {
            subset_stats.insert(e.path_in(backing).to_owned(), e.stat);
        }
    }

    // Changed entries get their fresh stat; unchanged entries reuse the old one.
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

    // Drop any stale cache-tree extension before persisting.
    new_index.remove_tree();
    new_index.write(Default::default())?;

    Ok(())
}
