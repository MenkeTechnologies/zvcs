use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::index::entry::{Flags, Mode, Stage, Stat};
use gix::objs::tree::EntryMode;
use gix::prelude::ObjectIdExt;
use gix::ObjectId;

/// git's `status_format` for `git commit`'s report (builtin/commit.c). `None` is
/// the unset default and the only value that still records a commit; every other
/// value implies `--dry-run`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusFormat {
    /// Unset — commit for real unless `--dry-run` was given.
    None,
    /// `--long` — git's default report shape.
    Long,
    /// `-s`/`--short`.
    Short,
    /// `--porcelain` (v1; commit's `--porcelain` takes no version).
    Porcelain,
}

/// git's `sign_commit` pointer as a tri-state: unspecified (so `commit.gpgSign`
/// decides), explicitly off (`--no-gpg-sign`), or on with an optional key id.
enum GpgSign {
    /// No `-S`/`--no-gpg-sign` on the command line.
    Unset,
    /// `--no-gpg-sign`, which also overrides `commit.gpgSign`.
    Off,
    /// `-S` / `-S<keyid>` / `--gpg-sign=<keyid>`.
    On(Option<String>),
}

/// A resolved gpg signing setup: the program (`gpg.program`, default `gpg`) and
/// the key (`-S<keyid>` else `user.signingKey`; `None` lets gpg pick its default).
struct Signer {
    /// The signing program git would exec — `gpg.program` or plain `gpg`.
    program: String,
    /// The key id passed as `-u`, when one is configured or was given.
    key: Option<String>,
}

impl Signer {
    /// Resolve the program and key from config, with `key` (from `-S<keyid>`)
    /// taking precedence over `user.signingKey`, exactly as git does.
    fn resolve(snap: &gix::config::Snapshot<'_>, key: Option<String>) -> Self {
        Signer {
            program: snap
                .string("gpg.program")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "gpg".to_string()),
            key: key.or_else(|| snap.string("user.signingKey").map(|v| v.to_string())),
        }
    }
}

/// Everything `dry_run_commit()` needs: the report shape plus which index the
/// report is taken against (`-a`, `-i`/`--include`, or a pathspec-limited commit).
struct DryRun {
    /// The resolved `status_format`.
    format: StatusFormat,
    /// `-z`/`--null`.
    null_term: bool,
    /// `-b`/`--branch`, unset when the config default should apply.
    branch_header: Option<bool>,
    /// `--[no-]ahead-behind`, unset when the config default should apply.
    ahead_behind: Option<bool>,
    /// The raw `-u`/`--untracked-files` argument, validated by the status engine.
    untracked: Option<String>,
    /// `-a`/`--all`.
    all: bool,
    /// `-i`/`--include`.
    include: bool,
    /// The pathspecs, if any.
    pathspecs: Vec<String>,
}

/// git's `enum commit_whence` (commit.h): where the commit being recorded came
/// from. Anything but [`Whence::Commit`] means an operation is in progress and
/// `git commit` is *concluding* it — which changes the parent list, the default
/// message, which options are legal, and what state is torn down afterwards.
///
/// `FROM_CHERRY_PICK_MULTI` is deliberately absent: `sequencer_determine_whence()`
/// assigns it when `.git/sequencer` exists and then unconditionally overwrites it
/// from the `if/else` immediately below, so in git 2.55.0 the value can never
/// reach `cmd_commit`. Porting the dead store would only add an unreachable arm.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Whence {
    /// `FROM_COMMIT` — an ordinary commit.
    Commit,
    /// `FROM_MERGE` — `MERGE_HEAD` exists; this commit concludes a merge.
    Merge,
    /// `FROM_CHERRY_PICK_SINGLE` — `CHERRY_PICK_HEAD` exists (cherry-pick or revert).
    CherryPick,
    /// `FROM_REBASE_PICK` — `CHERRY_PICK_HEAD` exists and equals `REBASE_HEAD`
    /// while a `rebase-merge` directory is present.
    RebasePick,
}

impl Whence {
    /// git's `is_from_cherry_pick()`.
    fn is_cherry_pick(self) -> bool {
        self == Whence::CherryPick
    }

    /// git's `is_from_rebase()`.
    fn is_rebase(self) -> bool {
        self == Whence::RebasePick
    }

    /// The noun git puts in "cannot do a partial commit during a %s.",
    /// "You are in the middle of a %s -- cannot amend." and friends.
    fn noun(self) -> &'static str {
        match self {
            Whence::Commit => "commit",
            Whence::Merge => "merge",
            Whence::CherryPick => "cherry-pick",
            Whence::RebasePick => "rebase",
        }
    }
}

/// git's `determine_whence()` (builtin/commit.c) plus `sequencer_determine_whence()`.
fn determine_whence(repo: &gix::Repository) -> Whence {
    let git_dir = repo.git_dir();
    if git_dir.join("MERGE_HEAD").exists() {
        return Whence::Merge;
    }
    let cherry = match read_state_oid(repo, "CHERRY_PICK_HEAD") {
        Some(id) => id,
        None => return Whence::Commit,
    };
    // `file_exists(rebase_path())` is `.git/rebase-merge`; `REBASE_HEAD` must name
    // the very commit being picked for this to be a rebase rather than a plain
    // cherry-pick that happens to run inside one.
    let in_rebase = git_dir.join("rebase-merge").exists()
        && read_state_oid(repo, "REBASE_HEAD") == Some(cherry);
    if in_rebase {
        Whence::RebasePick
    } else {
        Whence::CherryPick
    }
}

/// Resolve one of the sequencer's pseudo-refs (`CHERRY_PICK_HEAD`, `REVERT_HEAD`,
/// `REBASE_HEAD`, `AUTO_MERGE`) to an object id, or `None` when it does not exist.
///
/// git reaches these through the ref store with `REF_NO_DEREF`, so a loose file
/// holding a raw object id is the normal representation.
fn read_state_oid(repo: &gix::Repository, name: &str) -> Option<ObjectId> {
    // These are written as a bare loose file holding the id (that is what the ref
    // store produces for a root-level pseudo-ref, and what `cherry_pick` writes),
    // so read the file first and only then ask the ref store.
    if let Ok(text) = std::fs::read_to_string(repo.git_dir().join(name)) {
        if let Ok(id) = gix::ObjectId::from_hex(text.trim().as_bytes()) {
            return Some(id);
        }
    }
    repo.find_reference(name)
        .ok()
        .and_then(|mut r| r.peel_to_id().ok())
        .map(|id| id.detach())
}

/// Delete one of those pseudo-refs, reporting whether it had existed — git's
/// `refs_delete_ref(..., REF_NO_DEREF)`.
fn delete_state_ref(repo: &gix::Repository, name: &str) -> bool {
    let mut removed = std::fs::remove_file(repo.git_dir().join(name)).is_ok();
    if let Ok(reference) = repo.find_reference(name) {
        let current = reference.target().into_owned();
        removed |= repo
            .edit_reference(gix::refs::transaction::RefEdit {
                change: gix::refs::transaction::Change::Delete {
                    expected: gix::refs::transaction::PreviousValue::MustExistAndMatch(current),
                    log: gix::refs::transaction::RefLog::AndReference,
                },
                name: reference.name().to_owned(),
                deref: false,
            })
            .is_ok();
    }
    removed
}

/// git's `sequencer_post_commit_cleanup()` (sequencer.c): drop the pseudo-refs a
/// cherry-pick/revert left behind and, once the todo list is down to its final
/// entry, the sequencer directory with it.
fn sequencer_post_commit_cleanup(repo: &gix::Repository) -> Result<()> {
    let mut need_cleanup = delete_state_ref(repo, "CHERRY_PICK_HEAD");
    need_cleanup |= delete_state_ref(repo, "REVERT_HEAD");
    delete_state_ref(repo, "AUTO_MERGE");
    if !need_cleanup || !have_finished_the_last_pick(repo) {
        return Ok(());
    }
    // `sequencer_remove_state()`: the whole `.git/sequencer` directory goes.
    let dir = repo.git_dir().join("sequencer");
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// git's `have_finished_the_last_pick()`: true when `.git/sequencer/todo` holds
/// at most one line (the pick just concluded), false when it is missing entirely.
fn have_finished_the_last_pick(repo: &gix::Repository) -> bool {
    let Ok(buf) = std::fs::read(repo.git_dir().join("sequencer").join("todo")) else {
        return false;
    };
    match buf.iter().position(|&b| b == b'\n') {
        None => true,
        Some(eol) => eol + 1 >= buf.len(),
    }
}

/// git's `refresh_cache_or_die()` → `die_resolve_conflict("commit")`: the exact
/// output `git commit` produces while unmerged entries remain in the index.
///
/// The `U<TAB><path>` lines are `refresh_index()`'s `REFRESH_IN_PORCELAIN`
/// report and go to **stdout**, one per conflicted path; the diagnosis and the
/// `advice.resolveConflict` hint go to stderr, and the exit status is 128.
fn die_resolve_conflict(index: &gix::index::File) -> ExitCode {
    let backing = index.path_backing();
    let mut last: Option<&gix::bstr::BStr> = None;
    for entry in index.entries() {
        if entry.stage() == gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let path = entry.path_in(backing);
        // The index holds up to three stages per conflicted path; git skips
        // forward over the run so each path is reported once.
        if last == Some(path) {
            continue;
        }
        println!("U\t{path}");
        last = Some(path);
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());
    eprintln!("error: Committing is not possible because you have unmerged files.");
    crate::advice::Advice::ResolveConflict.advise_plain(
        "Fix them up in the work tree, and then use 'git add/rm <file>'\n\
         as appropriate to mark resolution and make a commit.",
    );
    eprintln!("fatal: Exiting because of an unresolved conflict.");
    ExitCode::from(128)
}

/// git's `apply_autostash_ref(r, "MERGE_AUTOSTASH", …)` — the last thing
/// `cmd_commit` does. `git merge --autostash` that stopped on a conflict parked
/// the dirty worktree under `MERGE_AUTOSTASH`; the commit that concludes the
/// merge puts it back.
///
/// The ref goes away either way: a clean apply reports `Applied autostash.`, and
/// a conflicting one hands the commit to `git stash store` so it stays reachable
/// through `refs/stash` (`apply_save_autostash_oid()`).
fn apply_merge_autostash(repo: &gix::Repository) -> Result<()> {
    let Some(stash) = read_state_oid(repo, "MERGE_AUTOSTASH") else {
        return Ok(());
    };
    let conflicts = super::stash::apply_autostash(repo, stash, true)?;
    if conflicts.is_empty() {
        eprintln!("Applied autostash.");
    } else {
        let args = ["store", "-m", "autostash", "-q", &stash.to_string()]
            .map(str::to_string)
            .to_vec();
        if super::stash::stash(&args).is_err() {
            eprintln!("error: cannot store {stash}");
        } else {
            eprintln!(
                "Your local changes are stashed, however applying them\n\
                 resulted in conflicts.  You can either resolve the conflicts\n\
                 and then discard the stash with \"git stash drop\", or, if you\n\
                 do not want to resolve them now, run \"git reset --hard\" and\n\
                 apply the local changes later by running \"git stash pop\"."
            );
        }
    }
    delete_state_ref(repo, "MERGE_AUTOSTASH");
    Ok(())
}

/// git's `get_merge_parent()` loop over `MERGE_HEAD`: one object id per line.
fn read_merge_heads(repo: &gix::Repository) -> Result<Vec<ObjectId>> {
    let path = repo.git_dir().join("MERGE_HEAD");
    let text = std::fs::read_to_string(&path)?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let id = repo
            .rev_parse_single(line)
            .map_err(|_| anyhow::anyhow!("Corrupt MERGE_HEAD file ({line})"))?;
        out.push(repo.find_commit(id.detach())?.id);
    }
    Ok(out)
}

/// git's `reduce_heads_replace()` on the parent list: drop every parent that is
/// already an ancestor of another, keeping the first occurrence's order.
///
/// Skipped when `MERGE_MODE` says `no-ff`, because the user asked for a merge
/// commit even where a fast-forward would have done.
fn reduce_heads(repo: &gix::Repository, parents: Vec<ObjectId>) -> Result<Vec<ObjectId>> {
    let mut kept: Vec<ObjectId> = Vec::new();
    for (i, cand) in parents.iter().enumerate() {
        if parents.iter().take(i).any(|p| p == cand) {
            continue;
        }
        let redundant = parents.iter().enumerate().any(|(j, other)| {
            i != j && other != cand && is_ancestor(repo, *cand, *other).unwrap_or(false)
        });
        if !redundant {
            kept.push(*cand);
        }
    }
    Ok(kept)
}

/// True when `ancestor` is reachable from `tip` (or is `tip` itself) — the same
/// merge-base test `in_merge_bases()` performs, which stops at the common
/// ancestor instead of walking the whole history.
fn is_ancestor(repo: &gix::Repository, ancestor: ObjectId, tip: ObjectId) -> Result<bool> {
    if ancestor == tip {
        return Ok(true);
    }
    Ok(matches!(repo.merge_base(ancestor, tip), Ok(base) if base.detach() == ancestor))
}

/// `git commit` — record a commit from the staged index.
///
/// Supported invocation forms (the ones the meta workflow relies on):
///   * `git commit -m <msg>` (repeatable; paragraphs joined by a blank line)
///   * `--message=<msg>` / `-m<msg>` (attached value)
///   * `-F <file>` / `--file=<file>` (message from a file; `-` is stdin)
///   * `-C <commit>` / `-c <commit>` (reuse a commit's message + author; `-c`
///     opens the editor), `--reset-author`, `--author=<ident>`, `--date=<date>`
///   * `--amend` (replace `HEAD`; `--no-edit` keeps its message)
///   * `--allow-empty`, `--allow-empty-message`, `-q`/`--quiet`
///   * `-a`/`--all` (auto-stage tracked modifications and deletions)
///   * bundled short flags, e.g. `-am <msg>` / `-qam <msg>` / `-C<commit>`
///
/// The tree is built from the current index (staging area), the commit is
/// written with `author`/`committer` from configuration, and `HEAD` is advanced
/// exactly like `git`: write-through to the branch it points at, or the detached
/// `HEAD` directly, with a matching reflog entry.
///
/// The summary line and short-stat output match stock `git commit` for the
/// common add/modify/delete/mode-change cases. Rename detection is NOT performed
/// (a rename is reported as a delete plus a create), and binary blobs contribute
/// `0` insertions/deletions to the short-stat, just as `git` does.
///
/// With no `-m`, the message is captured from an editor exactly as git does:
/// a template (`commit.template` plus a status header, unless `commit.status` is
/// false) is opened with the `GIT_EDITOR` → `core.editor` → `$VISUAL` →
/// `$EDITOR` editor, then cleaned up per `commit.cleanup` (default: strip
/// comment/blank lines) with the comment prefix taken from `core.commentString`
/// or `core.commentChar`.
///
/// `-s`/`--signoff` (`--no-signoff`) appends a `Signed-off-by:` trailer with the
/// committer identity, a faithful port of `append_signoff()`. `--squash <commit>`
/// and `--fixup <commit>` (including `--fixup=amend:<commit>`) build git's
/// autosquash-formatted message from the referenced commit.
/// `--trailer <token>[(=|:)<value>]` runs the message through the same engine
/// `git interpret-trailers --in-place --no-divider` uses, exactly as git spawns it.
///
/// `git commit [--only|-o] <paths>` (the default when paths are given) records a
/// pathspec-limited commit: the tree is HEAD's tree with only the listed paths
/// taken from the worktree, other paths' staged changes disregarded, and the same
/// paths are then staged into the real index. `-i`/`--include <paths>` instead
/// adds the listed paths to the index first and then commits the whole index.
/// `-a` together with paths (or with `-o`/`-i`) is refused, and `--amend` with
/// paths is allowed. `--pathspec-from-file=<file>` (`--pathspec-file-nul`) reads
/// the same pathspecs from a file or, for `-`, from stdin.
///
/// `--dry-run` (and the formats that imply it — `--short`, `--long`,
/// `--porcelain`, `-z`) prints the would-be commit's status through the very
/// engine `git status` uses and exits `0` when something is committable, `1`
/// when nothing is; `--branch`, `--ahead-behind` and `-u<mode>` tune that report.
/// The prepared index (`-a`, `-i`, `--only`) is installed for the report and the
/// real one restored afterward, so a dry run never changes the repository.
///
/// `--cleanup=<mode>` (`commit.cleanup`) selects git's message cleanup, resolved
/// against whether an editor is used, and `-t`/`--template` (`commit.template`)
/// seeds it — an unedited template aborts the commit exactly as git's
/// `template_untouched()` does. `-e`/`--edit` and `--no-edit` force the editor on
/// and off, `--status`/`--no-status` (`commit.status`) gate the commented status
/// block, and `-v`/`--verbose` (`commit.verbose`) appends the staged diff below a
/// scissors line. `-n`/`--no-verify` and `--verify` toggle the `pre-commit` and
/// `commit-msg` hooks; `--no-post-rewrite` suppresses the `post-rewrite` hook an
/// `--amend` otherwise fires. `-S`/`--gpg-sign[=<keyid>]` (`commit.gpgSign`,
/// `user.signingKey`, `gpg.program`) writes a `gpgsig` header.
///
/// `-p`/`--patch` stages through the hunk selector ([`super::add_patch`]) and
/// plain `--interactive` through the numbered menu ([`super::add_interactive`]),
/// with `-U`/`--unified` and `--inter-hunk-context` shaping the diff they show;
/// outside patch mode those two are refused, as git refuses them. The selection
/// is rolled back when the commit does not go through — see [`InteractiveStage`].
///
/// `--fixup=reword:` is still not backed and fails with a precise message rather
/// than silently doing the wrong thing.
///
/// A commit that *concludes an operation* — [`determine_whence`] — is not an
/// ordinary commit. Concluding a merge takes `HEAD` plus every id in `MERGE_HEAD`
/// as its parents (reduced with `reduce_heads_replace()` unless `MERGE_MODE` says
/// `no-ff`), defaults its message to `MERGE_MSG` (behind `SQUASH_MSG`, when a
/// `merge --squash` left one), is exempt from the nothing-to-commit guard, and
/// prints no diffstat. Concluding a cherry-pick or rebase pick keeps the picked
/// commit's authorship and writes a `commit (cherry-pick)`/`commit (rebase)`
/// reflog line. Afterwards the state is torn down exactly as git tears it down:
/// `CHERRY_PICK_HEAD`, `REVERT_HEAD` and `AUTO_MERGE` are deleted (with the
/// `sequencer` directory once the last pick is in), then `MERGE_HEAD`,
/// `MERGE_MSG`, `MERGE_MODE` and `SQUASH_MSG`; rerere records the resolutions;
/// and `MERGE_AUTOSTASH` is put back. `--amend` and a pathspec-limited commit are
/// both refused while an operation is in progress, and unmerged index entries
/// refuse the commit with git's `U<TAB><path>` report and exit 128.
pub fn commit(args: &[String]) -> Result<ExitCode> {
    // --- argument parsing ------------------------------------------------
    let mut messages: Vec<String> = Vec::new();
    let mut allow_empty = false;
    let mut allow_empty_message = false;
    let mut quiet = false;
    let mut all = false;
    // `--verify` / `-n`/`--no-verify`, last occurrence winning, gating the
    // `pre-commit` and `commit-msg` hooks.
    let mut verify = true;
    let mut amend = false;
    // git's tri-state `edit_flag`: `Some(true)` from `-e`/`--edit`, `Some(false)`
    // from `--no-edit`, `None` when unspecified (the message source decides).
    let mut edit_flag: Option<bool> = None;
    let mut reset_author = false;
    let mut author_arg: Option<String> = None;
    let mut date_arg: Option<String> = None;
    // `-C`/`-c` reuse an existing commit's message (and author); `-c` also opens
    // the editor. `-F` reads the message from a file. All are message *sources*
    // like `-m`, resolved once the repo is open.
    let mut reuse_arg: Option<String> = None;
    let mut reedit = false;
    let mut file_args: Vec<String> = Vec::new();
    // `-s`/`--signoff` adds a `Signed-off-by:` trailer with the committer ident;
    // `--squash`/`--fixup` build an autosquash-formatted message from a commit.
    let mut signoff = false;
    let mut squash_arg: Option<String> = None;
    let mut fixup_arg: Option<String> = None;
    // Pathspec-limited (git's default `--only`/`-o`) mode: the trailing `<paths>`
    // (bare positionals and everything after `--`). When any are given, the commit
    // tree is HEAD's tree with only these paths replaced by their worktree content.
    let mut pathspecs: Vec<String> = Vec::new();
    let mut positional_only = false;
    // `--dry-run` and the status-report options it drives. `status_format` other
    // than `None` implies a dry run, exactly as `parse_and_validate_options()`
    // does; `-z` promotes an unset/long format to porcelain first.
    let mut dry_run = false;
    let mut status_format = StatusFormat::None;
    let mut null_term = false;
    let mut branch_header: Option<bool> = None;
    let mut ahead_behind: Option<bool> = None;
    let mut untracked_arg: Option<String> = None;
    // `-o`/`--only` (the default when paths are given) vs `-i`/`--include`.
    let mut only_flag = false;
    let mut include_flag = false;
    // Message shaping: `--cleanup=<mode>`, `--trailer`, `-t`/`--template`,
    // `--status`, `-v`/`--verbose`.
    let mut cleanup_arg: Option<String> = None;
    let mut trailer_args: Vec<String> = Vec::new();
    let mut template_arg: Option<String> = None;
    let mut status_flag: Option<bool> = None;
    let mut verbose: Option<bool> = None;
    // `--no-post-rewrite` suppresses the `post-rewrite` hook an amend fires.
    let mut post_rewrite = true;
    let mut gpg_sign = GpgSign::Unset;
    // `--pathspec-from-file=<file>` (`-` = stdin) with `--pathspec-file-nul`.
    let mut pathspec_from_file: Option<String> = None;
    let mut pathspec_file_nul = false;
    // `-p`/`--patch` and `--interactive` (git's `patch_interactive` and
    // `interactive`; both `OPT_BOOL`, so the `--no-` forms clear them). They hand
    // staging to the hunk selector before the message is read.
    let mut patch_interactive = false;
    let mut interactive = false;
    // `-U`/`--unified` and `--inter-hunk-context` shape the selector's diff.
    // git's `commit` has no `--auto-advance`, unlike `add`/`reset`/`checkout`.
    let mut patch_opts = super::reset::PatchDiffOpts::without_auto_advance();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        // A value still owed to `-U`/`--unified`/`--inter-hunk-context` is taken
        // verbatim, even past `--`, the way parse-options takes it.
        if patch_opts.awaiting_value() || !positional_only {
            match patch_opts.take_arg(a) {
                Err(code) => return Ok(code),
                Ok(true) => {
                    i += 1;
                    continue;
                }
                Ok(false) => {}
            }
        }
        if positional_only {
            pathspecs.push(args[i].clone());
            i += 1;
            continue;
        }
        match a {
            "-m" | "--message" => {
                i += 1;
                let m = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("option `{a}` requires a value"))?;
                messages.push(m.clone());
            }
            "-F" | "--file" => {
                i += 1;
                file_args.push(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("option `{a}` requires a value"))?
                        .clone(),
                );
            }
            "-C" | "--reuse-message" => {
                i += 1;
                reuse_arg = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("option `{a}` requires a value"))?
                        .clone(),
                );
            }
            "-c" | "--reedit-message" => {
                i += 1;
                reuse_arg = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("option `{a}` requires a value"))?
                        .clone(),
                );
                reedit = true;
            }
            "--date" => {
                i += 1;
                date_arg = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("option `--date` requires a value"))?
                        .clone(),
                );
            }
            s if s.starts_with("--file=") => file_args.push(s["--file=".len()..].to_string()),
            s if s.starts_with("--reuse-message=") => {
                reuse_arg = Some(s["--reuse-message=".len()..].to_string())
            }
            s if s.starts_with("--reedit-message=") => {
                reuse_arg = Some(s["--reedit-message=".len()..].to_string());
                reedit = true;
            }
            s if s.starts_with("--date=") => date_arg = Some(s["--date=".len()..].to_string()),
            "--allow-empty" => allow_empty = true,
            "--allow-empty-message" => allow_empty_message = true,
            "-q" | "--quiet" => quiet = true,
            "-a" | "--all" => all = true,
            "--no-all" => all = false,
            // `-n`/`--no-verify` skips `pre-commit` + `commit-msg`; `--verify` is
            // its opposite, and the last one on the command line wins.
            "-n" | "--no-verify" => verify = false,
            "--verify" => verify = true,
            "-s" | "--signoff" => signoff = true,
            "--no-signoff" => signoff = false,
            "--squash" => {
                i += 1;
                squash_arg = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("option `--squash` requires a value"))?
                        .clone(),
                );
            }
            "--fixup" => {
                i += 1;
                fixup_arg = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("option `--fixup` requires a value"))?
                        .clone(),
                );
            }
            s if s.starts_with("--squash=") => {
                squash_arg = Some(s["--squash=".len()..].to_string())
            }
            s if s.starts_with("--fixup=") => fixup_arg = Some(s["--fixup=".len()..].to_string()),
            // `-v`/`--verbose` appends the staged diff below a scissors line in the
            // commit-message editor and truncates the message there afterward.
            "-v" | "--verbose" => verbose = Some(true),
            "--no-verbose" => verbose = Some(false),
            // Everything after `--` is a pathspec, even if it looks like a flag.
            "--" => positional_only = true,
            "--amend" => amend = true,
            "--no-amend" => amend = false,
            "-e" | "--edit" => edit_flag = Some(true),
            "--no-edit" => edit_flag = Some(false),
            "--reset-author" => reset_author = true,
            "--author" => {
                i += 1;
                author_arg = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("option `--author` requires a value"))?
                        .clone(),
                );
            }
            s if s.starts_with("--author=") => {
                author_arg = Some(s["--author=".len()..].to_string())
            }
            s if s.starts_with("--message=") => messages.push(s["--message=".len()..].to_string()),
            // --- the status-report family (git's `dry_run` + `status_format`) ---
            "--dry-run" => dry_run = true,
            "--no-dry-run" => dry_run = false,
            "--short" => status_format = StatusFormat::Short,
            "--long" => status_format = StatusFormat::Long,
            "--porcelain" => status_format = StatusFormat::Porcelain,
            // Every `--no-` form resets the format to git's `STATUS_FORMAT_NONE`,
            // which is also the "commit for real" state.
            "--no-short" | "--no-long" | "--no-porcelain" => status_format = StatusFormat::None,
            // Unlike `git status`, commit's `--porcelain` is a plain switch.
            s if s.starts_with("--porcelain=") => {
                anyhow::bail!("option `porcelain' takes no value")
            }
            "-z" | "--null" => null_term = true,
            "--no-null" => null_term = false,
            "-b" | "--branch" => branch_header = Some(true),
            "--no-branch" => branch_header = Some(false),
            "--ahead-behind" => ahead_behind = Some(true),
            "--no-ahead-behind" => ahead_behind = Some(false),
            // `-u`/`--untracked-files` is an OPTARG string defaulting to `all`;
            // the `--no-` form resets it to unspecified.
            "-u" | "--untracked-files" => untracked_arg = Some("all".to_string()),
            "--no-untracked-files" => untracked_arg = None,
            s if s.starts_with("--untracked-files=") => {
                untracked_arg = Some(s["--untracked-files=".len()..].to_string())
            }
            // --- what gets committed ------------------------------------------
            "-o" | "--only" => only_flag = true,
            "--no-only" => only_flag = false,
            "-i" | "--include" => include_flag = true,
            "--no-include" => include_flag = false,
            // Interactive staging: `-p` runs the hunk selector (`add-patch.c`),
            // plain `--interactive` runs the numbered menu (`add-interactive.c`).
            "-p" | "--patch" => patch_interactive = true,
            "--no-patch" => patch_interactive = false,
            "--interactive" => interactive = true,
            "--no-interactive" => interactive = false,
            "--pathspec-from-file" => {
                i += 1;
                pathspec_from_file = Some(
                    args.get(i)
                        .ok_or_else(|| {
                            anyhow::anyhow!("option `--pathspec-from-file` requires a value")
                        })?
                        .clone(),
                );
            }
            s if s.starts_with("--pathspec-from-file=") => {
                pathspec_from_file = Some(s["--pathspec-from-file=".len()..].to_string())
            }
            "--no-pathspec-from-file" => pathspec_from_file = None,
            "--pathspec-file-nul" => pathspec_file_nul = true,
            "--no-pathspec-file-nul" => pathspec_file_nul = false,
            // --- message shaping -----------------------------------------------
            "--cleanup" => {
                i += 1;
                cleanup_arg = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("option `--cleanup` requires a value"))?
                        .clone(),
                );
            }
            s if s.starts_with("--cleanup=") => {
                cleanup_arg = Some(s["--cleanup=".len()..].to_string())
            }
            "--no-cleanup" => cleanup_arg = None,
            "--trailer" => {
                i += 1;
                trailer_args.push(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("option `--trailer` requires a value"))?
                        .clone(),
                );
            }
            s if s.starts_with("--trailer=") => {
                trailer_args.push(s["--trailer=".len()..].to_string())
            }
            "--no-trailer" => trailer_args.clear(),
            "-t" | "--template" => {
                i += 1;
                template_arg = Some(
                    args.get(i)
                        .ok_or_else(|| anyhow::anyhow!("option `--template` requires a value"))?
                        .clone(),
                );
            }
            s if s.starts_with("--template=") => {
                template_arg = Some(s["--template=".len()..].to_string())
            }
            "--no-template" => template_arg = None,
            "--status" => status_flag = Some(true),
            "--no-status" => status_flag = Some(false),
            // --- hooks and signing ---------------------------------------------
            "--post-rewrite" => post_rewrite = true,
            "--no-post-rewrite" => post_rewrite = false,
            "-S" | "--gpg-sign" => gpg_sign = GpgSign::On(None),
            s if s.starts_with("--gpg-sign=") => {
                gpg_sign = GpgSign::On(Some(s["--gpg-sign=".len()..].to_string()))
            }
            "--no-gpg-sign" => gpg_sign = GpgSign::Off,
            s if s.starts_with("--") => anyhow::bail!("unsupported option `{s}`"),
            // `-S<keyid>` and `-u<mode>` take an *attached* value only, so they are
            // resolved before the generic short-cluster split below.
            s if s.starts_with("-S") && s.len() > 2 => {
                gpg_sign = GpgSign::On(Some(s[2..].to_string()))
            }
            s if s.starts_with("-u") && s.len() > 2 => {
                untracked_arg = Some(s[2..].to_string())
            }
            // A bundled short-flag cluster, e.g. `-am <msg>`, `-qam <msg>`,
            // `-amMSG`. git's parse-options treats every char as its own option;
            // the first one that takes a value consumes the rest of the cluster,
            // or the next argv element when the cluster ends there.
            s if s.len() > 1 && s.starts_with('-') => {
                let cluster = &s[1..];
                for (at, c) in cluster.char_indices() {
                    match c {
                        'a' => all = true,
                        'q' => quiet = true,
                        'n' => verify = false,
                        's' => signoff = true,
                        'v' => verbose = Some(true),
                        'e' => edit_flag = Some(true),
                        'o' => only_flag = true,
                        'i' => include_flag = true,
                        'p' => patch_interactive = true,
                        'z' => null_term = true,
                        'b' => branch_header = Some(true),
                        // Optional-value short flags: bare in a cluster they take
                        // their default, an attached value ends the cluster.
                        'u' | 'S' => {
                            let rest = &cluster[at + c.len_utf8()..];
                            match c {
                                'u' => {
                                    untracked_arg = Some(if rest.is_empty() {
                                        "all".to_string()
                                    } else {
                                        rest.to_string()
                                    })
                                }
                                _ => {
                                    gpg_sign = GpgSign::On(
                                        (!rest.is_empty()).then(|| rest.to_string()),
                                    )
                                }
                            }
                            if !rest.is_empty() {
                                break;
                            }
                        }
                        'm' | 'F' | 'C' | 'c' | 't' => {
                            // Value-taking flags consume the rest of the cluster,
                            // else the next argv element. `-c` also sets reedit.
                            let rest = &cluster[at + c.len_utf8()..];
                            let val = if rest.is_empty() {
                                i += 1;
                                args.get(i)
                                    .ok_or_else(|| {
                                        anyhow::anyhow!("option `-{c}` requires a value")
                                    })?
                                    .clone()
                            } else {
                                rest.to_string()
                            };
                            match c {
                                'm' => messages.push(val),
                                'F' => file_args.push(val),
                                'C' => reuse_arg = Some(val),
                                'c' => {
                                    reuse_arg = Some(val);
                                    reedit = true;
                                }
                                't' => template_arg = Some(val),
                                _ => unreachable!(),
                            }
                            break;
                        }
                        _ => anyhow::bail!("unsupported option `-{c}`"),
                    }
                }
            }
            // A bare positional argument is a pathspec (git's `--only` mode).
            _ => pathspecs.push(args[i].clone()),
        }
        i += 1;
    }

    // --- option validation (git's `parse_and_validate_options`) ----------
    if let Err(code) = patch_opts.finish() {
        return Ok(code);
    }
    // `-p` implies `--interactive`, and the four ways of choosing what to stage
    // are mutually exclusive (git's `die_for_incompatible_opt4(also, only, all,
    // interactive)`, which names them in that order).
    if patch_interactive {
        interactive = true;
    }
    if only_flag && include_flag {
        anyhow::bail!("options '-i/--include' and '-o/--only' cannot be used together");
    }
    if all && only_flag {
        anyhow::bail!("options '-o/--only' and '-a/--all' cannot be used together");
    }
    if all && include_flag {
        anyhow::bail!("options '-i/--include' and '-a/--all' cannot be used together");
    }
    if include_flag && interactive {
        anyhow::bail!(
            "options '-i/--include' and '--interactive/-p/--patch' cannot be used together"
        );
    }
    if only_flag && interactive {
        anyhow::bail!("options '-o/--only' and '--interactive/-p/--patch' cannot be used together");
    }
    if all && interactive {
        anyhow::bail!("options '-a/--all' and '--interactive/-p/--patch' cannot be used together");
    }

    // git's `prepare_index()` opens with the two `cannot be negative` fatals.
    if let Some(code) = patch_opts.reject_negative() {
        return Ok(code);
    }
    // `--pathspec-from-file` supplies the pathspec list instead of the command
    // line, so it is resolved before every pathspec-dependent check below.
    if pathspec_from_file.is_some() && !pathspecs.is_empty() {
        anyhow::bail!("'--pathspec-from-file' and pathspec arguments cannot be used together");
    }
    if pathspec_file_nul && pathspec_from_file.is_none() {
        anyhow::bail!("the option '--pathspec-file-nul' requires '--pathspec-from-file'");
    }
    if let Some(src) = &pathspec_from_file {
        if interactive {
            anyhow::bail!(
                "options '--pathspec-from-file' and '--interactive/--patch' cannot be used together"
            );
        }
        if all {
            anyhow::bail!("options '--pathspec-from-file' and '-a' cannot be used together");
        }
        pathspecs = read_pathspec_file(src, pathspec_file_nul)?;
    }
    if (only_flag || include_flag) && pathspecs.is_empty() {
        anyhow::bail!("No paths with --include/--only does not make sense.");
    }
    // Outside patch mode the two diff-shaping options have nothing to feed, and
    // git refuses the whole command rather than ignore them.
    if let Some(code) = patch_opts.require_patch_only(interactive, "--interactive/--patch") {
        return Ok(code);
    }
    // `git commit -a <paths>` is rejected outright, exactly as git does.
    if all && !pathspecs.is_empty() {
        anyhow::bail!("paths '{} ...' with -a does not make sense", pathspecs[0]);
    }
    // Pathspec-limited ("only") mode: build the commit tree from HEAD's tree with
    // only the listed paths taken from the worktree. `-i`/`--include` instead adds
    // the listed paths to the index and commits the whole index, so it is *not*
    // an only-mode commit even though it carries paths. Interactive staging is
    // likewise a whole-index commit: git's `prepare_index()` leaves its branch
    // with `commit_style = COMMIT_NORMAL`, so paths there only narrow the diff
    // the selector offers.
    let only_mode = !pathspecs.is_empty() && !include_flag && !interactive;

    // `-z` promotes an unset (or explicitly long) format to porcelain, and any
    // format at all implies a dry run — git's `finalize_deferred_config()` plus
    // the `status_format != STATUS_FORMAT_NONE` rule in cmd_commit.
    if null_term && matches!(status_format, StatusFormat::None | StatusFormat::Long) {
        status_format = StatusFormat::Porcelain;
    }
    if status_format != StatusFormat::None {
        dry_run = true;
    }

    // --- repository + serialized read-modify-write -----------------------
    let repo = gix::discover(".")?;
    // Serialize tree build + commit + HEAD update through the repo coordinator so
    // concurrent zvcs writers queue instead of racing. Held across the whole op —
    // except that `-p`/`--interactive` must run the selector *outside* the lane,
    // exactly as git's `prepare_index()` runs `interactive_add()` before it takes
    // the index lock. The selector hands each accepted hunk to a `git apply`
    // CHILD process, and a lane this process already holds is not reentrant across
    // a process boundary: the child would find it busy, queue itself as a job and
    // exit, and the whole selection would be silently dropped. It is re-taken the
    // moment the selector returns.
    let mut _lock = (!interactive).then(|| crate::lock::RepoLock::acquire(repo.git_dir()));

    // --- `determine_whence()` --------------------------------------------
    // A merge, cherry-pick, revert or rebase left in the index is what this
    // commit concludes; everything downstream (parents, default message, which
    // options are legal, what state is torn down) keys off this.
    let whence = determine_whence(&repo);

    // `parse_and_validate_options()`: an in-progress operation forbids `--amend`,
    // because the commit being replaced is not the one the operation is building.
    if amend && whence != Whence::Commit {
        anyhow::bail!("You are in the middle of a {} -- cannot amend.", whence.noun());
    }
    // `prepare_index()`: a pathspec-limited commit builds a tree that ignores the
    // rest of the index, which would silently drop the operation's other paths.
    if only_mode && whence != Whence::Commit {
        anyhow::bail!("cannot do a partial commit during a {}.", whence.noun());
    }

    // --- `-p`/`--interactive`: hand staging to the hunk selector ----------
    // git's `prepare_index()` runs `interactive_add()` before anything reads the
    // index, so `--dry-run` reaches it too and then throws the selection away
    // with the rest of the prepared index (`rollback_index_files()`); the guard
    // below lives to the end of this function and does exactly that unless the
    // commit succeeds.
    let mut interactive_stage = None;
    if interactive {
        let guard = InteractiveStage::hold(&repo)?;
        let status = if patch_interactive {
            super::add_patch::run_status(
                &repo,
                super::add_patch::Mode::Add,
                None,
                patch_opts.to_interactive(false),
                &pathspecs,
            )?
        } else {
            super::add_interactive::run_status(&repo, patch_opts.to_interactive(false), &pathspecs)?
        };
        if status != 0 {
            anyhow::bail!("interactive add failed");
        }
        interactive_stage = Some(guard);
        // The selector is done and its `apply` children have exited, so the lane
        // is safe to hold again for the tree build, the commit and the ref update.
        _lock = Some(crate::lock::RepoLock::acquire(repo.git_dir()));
    }

    // `--dry-run` returns before any message is read, any hook fires and any
    // object is written — git's `cmd_commit` branches to `dry_run_commit()` right
    // after option validation.
    if dry_run {
        if amend && repo.head()?.try_peel_to_id()?.is_none() {
            anyhow::bail!("You have nothing to amend.");
        }
        if amend {
            anyhow::bail!(
                "--dry-run with --amend is not ported: git reports against HEAD^, \
                 which the status engine cannot be pointed at"
            );
        }
        return dry_run_commit(
            &repo,
            &DryRun {
                format: status_format,
                null_term,
                branch_header,
                ahead_behind,
                untracked: untracked_arg.clone(),
                all,
                include: include_flag,
                pathspecs: pathspecs.clone(),
            },
        );
    }

    // `-F <file>` (repeatable) supplies the message from a file, joined with any
    // `-m` blocks in the order given; `-` reads stdin. Read here so it feeds the
    // same `from_flags`/no-editor path as `-m`.
    for f in &file_args {
        let content = if f == "-" {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
            s
        } else {
            std::fs::read_to_string(f)
                .map_err(|e| anyhow::anyhow!("could not read message file `{f}`: {e}"))?
        };
        messages.push(content);
    }

    // A `-m`/`--message`/`-F` value is validated now; without one, the message is
    // captured from the editor below (or reused via `-C`), but only once we know
    // there is something to commit (git opens the editor only then).
    let mut from_flags = !messages.is_empty();
    let mut message = messages.join("\n\n");
    if from_flags {
        if message.trim().is_empty() && !allow_empty_message {
            anyhow::bail!("empty commit message (use --allow-empty-message to override)");
        }
        // Match git's on-disk message, which is newline-terminated.
        if !message.ends_with('\n') {
            message.push('\n');
        }
    }

    // `-C`/`-c <commit>`: resolve the commit whose message and author are reused.
    let reuse_commit = match &reuse_arg {
        Some(spec) => Some(
            repo.find_commit(
                repo.rev_parse_single(spec.as_str())
                    .map_err(|e| anyhow::anyhow!("could not resolve `{spec}`: {e}"))?
                    .detach(),
            )?,
        ),
        None => None,
    };
    // `-C` (unlike `-c`) supplies the message directly, with no editor.
    if let Some(rc) = &reuse_commit {
        if !reedit && !from_flags {
            message = rc.message_raw()?.to_string();
            if !message.ends_with('\n') {
                message.push('\n');
            }
            from_flags = true;
        }
    }

    // --- `--squash` / `--fixup`: autosquash-formatted messages -----------
    // Port of the message shaping in `prepare_to_commit()`/`cmd_commit()`
    // (builtin/commit.c). The subject is git's folded `%s` of the referenced
    // commit. `--fixup` (default) writes `fixup! <subject>` and skips the
    // editor; `--squash` writes `squash! <subject>` and opens the editor unless
    // a `-m` body is given; `--fixup=amend:` writes `amend! <subject>` followed
    // by the whole original message and allows an empty change so a later
    // rebase can reword. `squash_fixup_seed`, when set, seeds the editor path.
    let mut squash_fixup_seed: Option<String> = None;
    if squash_arg.is_some() && fixup_arg.is_some() {
        anyhow::bail!("options '--squash' and '--fixup' cannot be used together");
    }
    if let Some(spec) = &squash_arg {
        if reuse_arg.is_some() {
            anyhow::bail!("--squash together with -c/-C is not supported");
        }
        let c = repo.find_commit(
            repo.rev_parse_single(spec.as_str())
                .map_err(|e| anyhow::anyhow!("could not lookup commit {spec}: {e}"))?
                .detach(),
        )?;
        let subject = folded_subject(c.message_raw()?.to_str_lossy().as_ref());
        if from_flags {
            // A `-m`/`-F` body follows the `squash!` subject line.
            message = format!("squash! {subject}\n\n{message}");
        } else {
            squash_fixup_seed = Some(format!("squash! {subject}\n\n"));
        }
    }
    if let Some(raw) = &fixup_arg {
        // `-c`/`-C`/`-F` are rejected with `--fixup` in every form.
        if reuse_arg.is_some() || !file_args.is_empty() {
            anyhow::bail!("options '-c/-C/-F' and '--fixup' cannot be used together");
        }
        // Parse `[(amend|reword):]<commit>`: only a leading run of alpha
        // characters immediately followed by `:` is treated as a suboption.
        let bytes = raw.as_bytes();
        let alpha = bytes.iter().take_while(|b| b.is_ascii_alphabetic()).count();
        let (fixup_spec, fixup_prefix): (&str, &str) =
            if alpha > 0 && bytes.get(alpha) == Some(&b':') {
                let sub = &raw[..alpha];
                let commit = &raw[alpha + 1..];
                match sub {
                    "amend" => (commit, "amend"),
                    "reword" => anyhow::bail!(
                        "--fixup=reword: requires a paths-limited (--only) commit, which is not ported"
                    ),
                    _ => anyhow::bail!("unknown option: --fixup={sub}:{commit}"),
                }
            } else {
                (raw.as_str(), "fixup")
            };
        let c = repo.find_commit(
            repo.rev_parse_single(fixup_spec)
                .map_err(|e| anyhow::anyhow!("could not lookup commit {fixup_spec}: {e}"))?
                .detach(),
        )?;
        let subject = folded_subject(c.message_raw()?.to_str_lossy().as_ref());
        if fixup_prefix == "fixup" {
            // Default `--fixup`: no editor; a `-m` body follows the subject line.
            message = if from_flags {
                format!("fixup! {subject}\n\n{message}")
            } else {
                format!("fixup! {subject}\n")
            };
            from_flags = true;
        } else {
            // `--fixup=amend:` — incompatible with `-m`, allows an empty change,
            // and carries the original message (its body only when the original
            // is itself an `amend!` commit, mirroring `prepare_amend_commit()`).
            if from_flags {
                anyhow::bail!("options '-m' and '--fixup=amend:<commit>' cannot be used together");
            }
            allow_empty = true;
            let orig = c.message_raw()?.to_str_lossy().into_owned();
            let carried = if subject_line(&orig).starts_with("amend!") {
                message_body(&orig)
            } else {
                orig
            };
            squash_fixup_seed = Some(format!("amend! {subject}\n\n{carried}"));
        }
    }
    // `--date=<date>` overrides the author date (git accepts fixed and relative
    // forms; `gix::date::parse` covers the same grammar).
    let date_override: Option<gix::date::Time> = match &date_arg {
        Some(d) => Some(
            gix::date::parse(d, Some(std::time::SystemTime::now()))
                .map_err(|e| anyhow::anyhow!("invalid date format `{d}`: {e}"))?,
        ),
        None => None,
    };

    let hash = repo.object_hash();

    // --- `-a`/`--all`: auto-stage tracked modifications and deletions -----
    // Runs under the same lock, and writes the index through before the tree is
    // built so the on-disk index and the commit agree even if we bail later.
    if all {
        stage_tracked_changes(&repo)?;
    }
    // --- `-i`/`--include <paths>`: stage the named paths, then commit it all ---
    // git's `prepare_index` treats `also && pathspec.nr` exactly like `-a`: the
    // paths are added to the real index up front and the commit is a normal,
    // whole-index commit afterward.
    if include_flag {
        let mut index = open_or_empty_index(&repo)?;
        include_stage(&repo, &pathspecs, &index)?.apply_to(&mut index);
        index.write(gix::index::write::Options::default())?;
    }

    // --- build a tree object from the index ------------------------------
    // A freshly-init'd repo has no index file yet. `open_index` errors on the
    // missing file, so treat its absence as an empty index — git's root empty
    // commit (`commit --allow-empty` on a fresh repo) then produces the empty tree
    // instead of failing with "opening the index: No such file or directory".
    // `open_index`'s Err variant is large; boxing it would churn every call site.
    #[allow(clippy::result_large_err)]
    let index = repo
        .index_path()
        .exists()
        .then(|| repo.open_index())
        .transpose()?;

    // Refuse while conflicts are staged, exactly as git does — `refresh_cache_or_die()`
    // reports every unmerged path and then `die_resolve_conflict("commit")`.
    if let Some(index) = &index {
        if index
            .entries()
            .iter()
            .any(|e| e.stage() != gix::index::entry::Stage::Unconflicted)
        {
            return Ok(die_resolve_conflict(index));
        }
    }

    // `pre-commit` runs before the commit is built; a non-zero exit aborts it
    // (the hook prints its own diagnostics, so we exit quietly). `--no-verify`
    // skips it, as it does `commit-msg`.
    if verify && !crate::hooks::run(&repo, "pre-commit", &[], None)? {
        return Ok(ExitCode::from(1));
    }

    // Feed every index entry into the plumbing tree editor, which builds the
    // nested trees in canonical (git) order and writes them to the odb. The
    // high-level `Repository::edit_tree` wrapper is gated behind the `tree-editor`
    // feature, so the editor is constructed directly over the public object
    // database handle instead. With no index (fresh repo) the editor stays empty
    // and writes the empty tree.
    //
    // In pathspec-limited ("only") mode the tree comes from HEAD's tree with only
    // the listed paths swapped for their worktree content instead — see
    // `build_only_mode_tree`, which also stages those paths into the real index.
    let (tree_id, new_entries): (ObjectId, Vec<(BString, EntryMode, ObjectId)>) = if only_mode {
        build_only_mode_tree(&repo, &pathspecs)?
    } else {
        let mut editor = gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, hash);
        // Snapshot (path, mode, id) per staged file for the summary/short-stat below.
        let mut new_entries: Vec<(BString, EntryMode, ObjectId)> = Vec::new();
        if let Some(index) = &index {
            let backing = index.path_backing();
            new_entries.reserve(index.entries().len());
            for entry in index.entries() {
                let path = entry.path_in(backing);
                let mode = entry.mode.to_tree_entry_mode().ok_or_else(|| {
                    anyhow::anyhow!("index entry `{path}` has an unrepresentable mode")
                })?;
                editor.upsert(
                    path.split(|&b| b == b'/').map(|c| c.as_bstr()),
                    mode.kind(),
                    entry.id,
                )?;
                new_entries.push((path.to_owned(), mode, entry.id));
            }
        }
        let tree_id = editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?;
        (tree_id, new_entries)
    };

    // --- parents ---------------------------------------------------------
    // `--amend` replaces HEAD: the new commit takes HEAD's *parents*, and the
    // summary/nothing-to-commit checks compare against HEAD's first parent tree,
    // not HEAD itself.
    let mut head = repo.head()?;
    let head_tip = head.try_peel_to_id()?.map(|id| id.detach());
    let amend_head = if amend {
        let hid = head_tip.ok_or_else(|| anyhow::anyhow!("You have nothing to amend."))?;
        Some(repo.find_commit(hid)?)
    } else {
        None
    };
    // Concluding a merge appends every id in `MERGE_HEAD` after `HEAD`, so the
    // commit records *both* sides. Without this the second parent is silently
    // dropped and the merge never happened as far as history is concerned.
    let parents: Vec<ObjectId> = match &amend_head {
        Some(hc) => hc.parent_ids().map(|id| id.detach()).collect(),
        None if whence == Whence::Merge => {
            let mut p: Vec<ObjectId> = head_tip.into_iter().collect();
            p.extend(read_merge_heads(&repo)?);
            // `MERGE_MODE` holding `no-ff` means the user asked for a merge commit
            // even where one side already contains the other, so the redundant
            // parent is kept; otherwise `reduce_heads_replace()` prunes it.
            let no_ff = std::fs::read(repo.git_dir().join("MERGE_MODE"))
                .map(|b| b == b"no-ff")
                .unwrap_or(false);
            if no_ff { p } else { reduce_heads(&repo, p)? }
        }
        None => head_tip.into_iter().collect(),
    };
    let is_root = parents.is_empty();
    // git's `log_tree_commit()` prints no diff for a commit with several parents,
    // so `print_commit_summary()` degenerates to the headline for a merge.
    let is_merge_commit = parents.len() > 1;

    let parent_tree_id = match parents.first() {
        Some(p) => Some(repo.find_commit(*p)?.tree_id()?.detach()),
        None => None,
    };

    // --- nothing-to-commit guard -----------------------------------------
    let unchanged = match parent_tree_id {
        Some(pt) => pt == tree_id,
        None => tree_id == ObjectId::empty_tree(hash),
    };
    // `--amend` always produces a new commit (a message- or author-only amend is
    // valid), so it is exempt from the empty-change guard. So is concluding a
    // merge — git's `!committable && whence != FROM_MERGE` — because resolving
    // every conflict back to `HEAD`'s content still has to record the merge.
    if unchanged && !allow_empty && whence != Whence::Merge {
        if amend {
            // git refuses an amend whose result would be empty (tree unchanged
            // from the parent) unless --allow-empty, with its own message.
            anyhow::bail!(
                "You asked to amend the most recent commit, but doing so would make\n\
                 it empty. You can repeat your command with --allow-empty, or you can\n\
                 remove the commit entirely with \"git reset HEAD^\"."
            );
        }
        // A cherry-pick or rebase pick whose conflict resolution left nothing to
        // record is a distinct situation from "you staged nothing": the pick has
        // to be either recorded empty or skipped, and git says which.
        if whence.is_cherry_pick() || whence.is_rebase() {
            eprint!(
                "The previous cherry-pick is now empty, possibly due to conflict resolution.\n\
                 If you wish to commit it anyway, use:\n\
                 \n    \
                 git commit --allow-empty\n\
                 \n"
            );
            if whence.is_rebase() {
                eprintln!("Otherwise, please use 'git rebase --skip'");
            } else {
                eprintln!("Otherwise, please use 'git cherry-pick --skip'");
            }
            return Ok(ExitCode::from(1));
        }
        anyhow::bail!("nothing to commit (no changes staged)");
    }

    // --- message: `prepare_to_commit()` -----------------------------------
    // git decides *once* whether an editor is used: a `-m`/`-F`/`-C` message
    // source turns it off, then an explicit `-e`/`--no-edit` overrides that. The
    // answer also picks the default cleanup mode, so it is computed first.
    let no_edit = edit_flag == Some(false);
    let use_editor = match edit_flag {
        Some(v) => v,
        None => !from_flags,
    };
    let snap = repo.config_snapshot();
    let cleanup = resolve_cleanup(cleanup_arg.as_deref(), &snap, use_editor)?;
    let comment = comment_prefix(&snap);
    // `-v`/`--verbose` (`commit.verbose`) appends the staged diff under a cut line.
    let verbose = verbose.unwrap_or_else(|| snap.boolean("commit.verbose") == Some(true));
    // `--status`/`--no-status`, defaulting to `commit.status` (git's `include_status`).
    let include_status = status_flag.unwrap_or_else(|| snap.boolean("commit.status") != Some(false));
    // `-t`/`--template <file>` beats `commit.template`; both seed the buffer and
    // both arm git's `template_untouched()` abort.
    let template_file: Option<std::path::PathBuf> = match &template_arg {
        Some(t) => Some(expand_tilde(t)),
        None => snap.string("commit.template").map(|v| expand_tilde(&v.to_string())),
    };

    // `prepare_to_commit()`'s message sources that sit below `-m`/`-F`/`-C`/
    // `--fixup` and above `commit.template`: `MERGE_MSG` — git's own
    // "Merge branch ..." headline plus the commented conflict list — with
    // `SQUASH_MSG` prepended when a `merge --squash` produced one, or `SQUASH_MSG`
    // on its own. Without this a concluded merge would be committed under an
    // empty (or template) message rather than the one the merge prepared.
    let merge_msg = std::fs::read_to_string(repo.git_dir().join("MERGE_MSG")).ok();
    let squash_msg = std::fs::read_to_string(repo.git_dir().join("SQUASH_MSG")).ok();
    let merge_msg_seed: Option<String> = match (&merge_msg, &squash_msg) {
        (Some(m), Some(s)) => Some(format!("{s}{m}")),
        (Some(m), None) => Some(m.clone()),
        (None, Some(s)) => Some(s.clone()),
        (None, None) => None,
    };

    // The buffer git hands the editor (and, without one, the message itself).
    let mut buf = if from_flags {
        message.clone()
    } else if amend && no_edit {
        let mut m = amend_head
            .as_ref()
            .expect("amend implies HEAD")
            .message_raw()?
            .to_string();
        if !m.ends_with('\n') {
            m.push('\n');
        }
        m
    } else if let Some(s) = &squash_fixup_seed {
        s.clone()
    } else if let Some(rc) = &reuse_commit {
        rc.message_raw()?.to_string()
    } else if amend {
        amend_head
            .as_ref()
            .expect("amend implies HEAD")
            .message_raw()?
            .to_string()
    } else if let Some(m) = &merge_msg_seed {
        m.clone()
    } else if let Some(path) = &template_file {
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("could not read commit template '{}': {e}", path.display()))?
    } else {
        String::new()
    };
    // The template text as git compares it in `template_untouched()`: the file's
    // contents cleaned with the same mode, and only when it actually seeded `buf`.
    let template_seed: Option<String> = match (&template_file, from_flags) {
        (Some(path), false)
            if squash_fixup_seed.is_none()
                && reuse_commit.is_none()
                && !amend
                && merge_msg_seed.is_none() =>
        {
            Some(cleanup_message(
                &std::fs::read_to_string(path).unwrap_or_default(),
                &comment,
                cleanup,
                false,
            ))
        }
        _ => None,
    };

    // `-s`/`--signoff` appends `Signed-off-by:` *before* the buffer is written, so
    // the editor and the `--trailer` pass both see it — `append_signoff()`
    // (sequencer.c) called from `prepare_to_commit()`.
    if signoff {
        let committer = repo
            .committer()
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("unable to determine committer identity"))?;
        let ident = format!("{} <{}>", committer.name, committer.email);
        append_signoff(&mut buf, &ident);
    }

    // The commented help + status block, and the `-v` diff below the cut line, go
    // into the editor buffer only — git gates both on `use_editor && include_status`.
    let msg_path = repo.git_dir().join("COMMIT_EDITMSG");
    if use_editor && include_status {
        if !buf.is_empty() && !buf.ends_with('\n') {
            buf.push('\n');
        }
        buf.push_str(&editor_status_block(&repo, is_root, &comment, cleanup, whence)?);
    }
    std::fs::write(&msg_path, &buf)?;
    if use_editor && include_status && verbose {
        append_verbose_diff(&repo, &msg_path, cleanup)?;
    }

    // `--trailer <token>[(=|:)<value>]`: git runs
    // `git interpret-trailers --in-place --no-divider <COMMIT_EDITMSG> <args>`;
    // we call the very same implementation in-process.
    if !trailer_args.is_empty() {
        apply_trailers(&msg_path, &trailer_args)?;
    }

    if use_editor {
        launch_editor(&snap, &msg_path)?;
    }
    message = cleanup_message(&std::fs::read_to_string(&msg_path)?, &comment, cleanup, verbose);

    // An untouched template aborts the commit — `template_untouched()`, which
    // compares the cleaned-up template against the cleaned-up result.
    if !allow_empty_message {
        if let Some(tmpl) = &template_seed {
            if template_untouched(&message, tmpl, cleanup, &comment) {
                eprintln!("Aborting commit; you did not edit the message.");
                return Ok(ExitCode::from(1));
            }
        }
    }
    if message.trim().is_empty() && !allow_empty_message {
        if from_flags {
            anyhow::bail!("empty commit message (use --allow-empty-message to override)");
        }
        anyhow::bail!("Aborting commit due to empty commit message.");
    }
    if !message.is_empty() && !message.ends_with('\n') {
        message.push('\n');
    }

    // `commit-msg` gets the message file and may rewrite it (e.g. add a trailer);
    // a non-zero exit aborts. Re-read afterward to pick up any edits.
    if verify {
        std::fs::write(&msg_path, &message)?;
        let arg = msg_path.to_string_lossy().into_owned();
        if !crate::hooks::run(&repo, "commit-msg", &[&arg], None)? {
            return Ok(ExitCode::from(1));
        }
        message = std::fs::read_to_string(&msg_path)?;
    }
    let subject = message.lines().next().unwrap_or("").to_string();

    // `--author="Name <email>"` overrides the author identity. The author *date*
    // is unchanged: HEAD's on an amend (git preserves it), the configured author
    // time (now / GIT_AUTHOR_DATE) on a new commit.
    let author_override: Option<(String, String)> = match &author_arg {
        Some(a) => Some(parse_author_ident(a)?),
        None => None,
    };

    // The effective author identity, computed once as an owned signature so its
    // parts outlive the write. Precedence for the base: `--reset-author` → config
    // identity; `-C`/`-c` → the reused commit; `--amend` → HEAD; else config.
    // `--author` then swaps name/email, `--date` the time. `None` means no
    // override — the plain `repo.commit()` fast path (config author + canonical
    // reflog) runs unchanged, so a bare `git commit` is byte-for-byte as before.
    // Concluding a cherry-pick, revert or rebase pick keeps the *picked* commit's
    // authorship — git's `author_message = "CHERRY_PICK_HEAD"`, which outranks
    // `-C`/`-c` and is disarmed only by `--reset-author`.
    let cherry_author: Option<gix::actor::Signature> =
        match (whence.is_cherry_pick() || whence.is_rebase()) && !reset_author {
            true => match read_state_oid(&repo, "CHERRY_PICK_HEAD") {
                Some(id) => Some(repo.find_commit(id)?.author()?.to_owned()?),
                None => None,
            },
            false => None,
        };
    let needs_author = amend
        || reset_author
        || author_override.is_some()
        || date_override.is_some()
        || reuse_commit.is_some()
        || cherry_author.is_some();
    let author_owned: Option<gix::actor::Signature> = if needs_author {
        let cfg_author = || -> Result<gix::actor::Signature> {
            Ok(repo
                .author()
                .transpose()?
                .ok_or_else(|| anyhow::anyhow!("unable to determine author identity"))?
                .to_owned()?)
        };
        let mut base = if reset_author {
            cfg_author()?
        } else if let Some(a) = &cherry_author {
            a.clone()
        } else if let Some(rc) = &reuse_commit {
            rc.author()?.to_owned()?
        } else if let Some(hc) = &amend_head {
            hc.author()?.to_owned()?
        } else {
            cfg_author()?
        };
        if let Some((name, email)) = &author_override {
            base.name = name.as_str().into();
            base.email = email.as_str().into();
        }
        if let Some(t) = date_override {
            base.time = t;
        }
        Some(base)
    } else {
        None
    };
    let committer_owned = || -> Result<gix::actor::Signature> {
        Ok(repo
            .committer()
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("unable to determine committer identity"))?
            .to_owned()?)
    };

    // `-S`/`--gpg-sign[=<keyid>]` (or `commit.gpgSign`) makes the object carry a
    // `gpgsig` header; the key is the flag's, else `user.signingKey`, and the
    // program `gpg.program`. `None` leaves the untouched `Repository::commit`
    // fast paths in charge, so an unsigned commit is byte-for-byte as before.
    let signer: Option<Signer> = match gpg_sign {
        GpgSign::Off => None,
        GpgSign::Unset if snap.boolean("commit.gpgSign") != Some(true) => None,
        GpgSign::Unset => Some(Signer::resolve(&snap, None)),
        GpgSign::On(key) => Some(Signer::resolve(&snap, key)),
    };

    // git's `reflog_msg`: "commit", "commit (initial)", "commit (amend)",
    // "commit (merge)", "commit (cherry-pick)" or "commit (rebase)". gix derives
    // the first four from the parent count on its own, so only the sequencer's two
    // need to be supplied — and supplying one forces the explicit write path below.
    let reflog_override: Option<String> = if whence.is_cherry_pick() {
        Some(format!("commit (cherry-pick): {subject}"))
    } else if whence.is_rebase() {
        Some(format!("commit (rebase): {subject}"))
    } else {
        None
    };

    // --- write the commit and advance HEAD -------------------------------
    let commit_id = if amend {
        // `--amend`: `Repository::commit`'s ref update requires the ref to equal
        // the new commit's first parent, which is false for an amend (HEAD points
        // at the commit being replaced, not its parent), so write the object with
        // `new_commit_as` and move HEAD ourselves, gating on HEAD's current tip
        // and writing git's `commit (amend):` reflog line.
        let author = author_owned.as_ref().expect("amend computes an author");
        let committer = committer_owned()?;
        let new: ObjectId = write_commit_object(
            &repo,
            &committer,
            author,
            &message,
            tree_id,
            parents,
            signer.as_ref(),
        )?;
        let prev = head_tip.expect("amend implies HEAD");
        repo.edit_reference(gix::refs::transaction::RefEdit {
            change: gix::refs::transaction::Change::Update {
                log: gix::refs::transaction::LogChange {
                    mode: gix::refs::transaction::RefLog::AndReference,
                    force_create_reflog: false,
                    message: format!("commit (amend): {subject}").into(),
                },
                expected: gix::refs::transaction::PreviousValue::MustExistAndMatch(
                    gix::refs::Target::Object(prev),
                ),
                new: gix::refs::Target::Object(new),
            },
            name: "HEAD"
                .try_into()
                .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
            deref: true,
        })?;
        new.attach(&repo)
    } else if signer.is_some() || reflog_override.is_some() {
        // A signed commit needs the `gpgsig` header, which `Repository::commit`
        // cannot carry, and a sequencer commit needs its own reflog wording; both
        // write the object here and advance `HEAD` themselves, otherwise with
        // gix's `commit`/`commit (initial)`/`commit (merge)` line — the same
        // wording and the same first-parent safety check the fast path uses.
        let committer = committer_owned()?;
        let author = match &author_owned {
            Some(a) => a.clone(),
            None => repo
                .author()
                .transpose()?
                .ok_or_else(|| anyhow::anyhow!("unable to determine author identity"))?
                .to_owned()?,
        };
        let parent_count = parents.len();
        let first_parent = parents.first().copied();
        let new = write_commit_object(
            &repo,
            &committer,
            &author,
            &message,
            tree_id,
            parents,
            signer.as_ref(),
        )?;
        repo.edit_reference(gix::refs::transaction::RefEdit {
            change: gix::refs::transaction::Change::Update {
                log: gix::refs::transaction::LogChange {
                    mode: gix::refs::transaction::RefLog::AndReference,
                    force_create_reflog: false,
                    message: match &reflog_override {
                        Some(m) => m.as_str().into(),
                        None => gix::reference::log::message(
                            "commit",
                            message.as_str().into(),
                            parent_count,
                        ),
                    },
                },
                expected: match first_parent {
                    Some(p) => gix::refs::transaction::PreviousValue::MustExistAndMatch(
                        gix::refs::Target::Object(p),
                    ),
                    None => gix::refs::transaction::PreviousValue::MustNotExist,
                },
                new: gix::refs::Target::Object(new),
            },
            name: "HEAD"
                .try_into()
                .map_err(|e| anyhow::anyhow!("invalid ref name HEAD: {e}"))?,
            deref: true,
        })?;
        new.attach(&repo)
    } else if let Some(author) = &author_owned {
        // A normal commit with an author override (`-C`/`-c`/`--author`/`--date`/
        // `--reset-author`): the config committer, the computed author. Drop to
        // `commit_as` to inject the override.
        let committer = committer_owned()?;
        repo.commit_as(
            committer.to_ref(&mut gix::date::parse::TimeBuf::default()),
            author.to_ref(&mut gix::date::parse::TimeBuf::default()),
            "HEAD",
            &message,
            tree_id,
            parents,
        )?
    } else {
        // `Repository::commit` writes the commit object, then updates `HEAD`
        // (write-through to its branch, or the detached ref) with the canonical
        // `commit`/`commit (initial)` reflog message, requiring the first parent
        // to be the current tip — the same ref-safety check git performs.
        repo.commit("HEAD", &message, tree_id, parents)?
    };

    // The commit is in the object store and `HEAD` points at it, which is git's
    // `commit_index_files()` moment: the prepared index becomes the real one and
    // an interactive selection is no longer rolled back.
    if let Some(stage) = &mut interactive_stage {
        stage.keep();
    }

    // The operation this commit concluded is over: drop the sequencer pseudo-refs
    // (and its todo directory once the last pick is in), then the merge state
    // files. Leaving `MERGE_HEAD` behind is what makes the next `git merge` die
    // with "You have not concluded your merge".
    sequencer_post_commit_cleanup(&repo)?;
    for name in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "SQUASH_MSG"] {
        let path = repo.git_dir().join(name);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }

    // `repo_rerere()` — the resolutions the user just staged become postimages, so
    // the same conflict replays automatically next time. Run here, after the index
    // is committed, exactly where `cmd_commit` calls it; `MERGE_RR` (which names
    // the conflict ids) deliberately survives the teardown above so this can pair
    // each resolved path with the preimage recorded when the conflict appeared.
    //
    // Guarded on the index file existing: git's `rerere()` reaches the index
    // through `repo_read_index()`, which yields an *empty* index when the file is
    // absent, while `rerere::repo_rerere` opens it and errors out. A repo with no
    // index file has no unmerged entries and so nothing for rerere to do, which
    // makes the guard the same no-op — but it belongs in `rerere.rs`, whose
    // `open_index()` calls should tolerate a missing file the way git's do.
    if repo.index_path().exists() {
        super::rerere::repo_rerere(&repo, None)?;
    }

    // `--amend` rewrites a commit, so git notifies `post-rewrite` with the
    // `amend` mode and one `<old-sha1> SP <new-sha1>` line on stdin;
    // `--no-post-rewrite` suppresses it. Its exit status is ignored.
    if amend && post_rewrite {
        if let Some(prev) = head_tip {
            let payload = format!("{} {}\n", prev, commit_id.detach());
            let _ = crate::hooks::run(&repo, "post-rewrite", &["amend"], Some(payload.as_bytes()));
        }
    }

    // `post-commit` is a notification hook: it runs after the commit regardless of
    // `--no-verify`, and its exit status is ignored.
    let _ = crate::hooks::run(&repo, "post-commit", &[], None);

    // `print_commit_summary()`, skipped by `-q`. It is the last thing before
    // `apply_autostash_ref()`, so the block is exited rather than returned from.
    'summary: {
    if quiet {
        break 'summary;
    }

    // --- summary line ----------------------------------------------------
    let short = commit_id.shorten_or_id();
    let branch_label = match repo.head_name()? {
        Some(name) => name.shorten().to_string(),
        None => "detached HEAD".to_string(),
    };
    let root_marker = if is_root { " (root-commit)" } else { "" };
    println!("[{branch_label}{root_marker} {short}] {subject}");

    // git prints ` Author:` when the author identity differs from the
    // committer's (as `--author` and `--amend`-preserved authors do), and
    // ` Date:` when the author date differs from the committer date.
    let written = repo.find_commit(commit_id.detach())?;
    let author = written.author()?;
    let committer = written.committer()?;
    if author.name != committer.name || author.email != committer.email {
        println!(" Author: {} <{}>", author.name, author.email);
    }
    // git's `author_date_is_interesting()` — `author_message || force_date`. The
    // author date is shown whenever it came from somewhere other than the clock:
    // a reused message (`-C`/`-c`), an amend, the commit a pick is replaying, or
    // `--date`. It is *not* inferred from the two dates differing, so a pick whose
    // author second happens to equal the committer's still prints the line.
    let author_date_is_interesting = date_override.is_some()
        || (!reset_author && (reuse_commit.is_some() || amend || cherry_author.is_some()));
    if author_date_is_interesting {
        let a_time = author.time()?;
        let dt = a_time
            .format(gix::date::time::format::DEFAULT)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        println!(" Date: {dt}");
    }

    // --- short-stat + create/delete/mode-change summary ------------------
    // `log_tree_diff()` bails out on a commit with more than one parent unless a
    // combined-diff mode is asked for, which `print_commit_summary()` never does,
    // so a merge prints its headline and nothing else.
    if is_merge_commit {
        break 'summary;
    }
    // Old file set (path -> mode, id) flattened from the parent tree; empty for
    // the root commit.
    let mut old_entries: HashMap<BString, (EntryMode, ObjectId)> = HashMap::new();
    if let Some(pt) = parent_tree_id {
        let old_index = repo.index_from_tree(&pt)?;
        let old_backing = old_index.path_backing();
        for e in old_index.entries() {
            if let Some(m) = e.mode.to_tree_entry_mode() {
                old_entries.insert(e.path_in(old_backing).to_owned(), (m, e.id));
            }
        }
    }
    let new_paths: HashSet<&BString> = new_entries.iter().map(|(p, _, _)| p).collect();

    // File-level change count (git's "N files changed"), including binaries and
    // pure mode changes; renames are counted as a delete plus a create.
    let mut files_changed: u64 = 0;
    let mut summary: Vec<(BString, String)> = Vec::new();
    for (path, mode, id) in &new_entries {
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

    // Line counts from a real tree-to-tree blob diff (rename detection off, to
    // keep the file accounting consistent with the count above).
    let new_tree = repo.find_tree(tree_id)?;
    let old_tree = match parent_tree_id {
        Some(pt) => repo.find_tree(pt)?,
        None => repo.empty_tree(),
    };
    let mut platform = old_tree.changes()?;
    platform.options(|opts| {
        opts.track_rewrites(None);
    });
    let stats = platform.stats(&new_tree)?;

    // git prints the diff block only when something actually changed.
    if files_changed > 0 {
        let ins = stats.lines_added;
        let del = stats.lines_removed;
        let mut line = format!(" {files_changed} file{} changed", plural(files_changed));
        // git shows the insertion clause unless there are only deletions, and the
        // deletion clause unless there are only insertions.
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
    } // 'summary

    // The merge is concluded, so the worktree `merge --autostash` put aside comes
    // back — git's very last act in `cmd_commit`.
    apply_merge_autostash(&repo)?;

    Ok(ExitCode::SUCCESS)
}

/// `--pathspec-from-file=<file>` — the pathspec list read from a file, or from
/// stdin for `-`. Entries are separated by `NUL` with `--pathspec-file-nul` and
/// by newlines otherwise; git also drops a trailing `\r` from the line form.
fn read_pathspec_file(src: &str, nul: bool) -> Result<Vec<String>> {
    let raw = if src == "-" {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        std::fs::read(src)
            .map_err(|e| anyhow::anyhow!("could not open '{src}' for reading: {e}"))?
    };
    let sep = if nul { b'\0' } else { b'\n' };
    Ok(raw
        .split(|&b| b == sep)
        .map(|s| {
            let s = if !nul { s.strip_suffix(b"\r").unwrap_or(s) } else { s };
            String::from_utf8_lossy(s).into_owned()
        })
        .filter(|s| !s.is_empty())
        .collect())
}

/// Open the on-disk index, or an empty one when the repo has never had a file
/// (a freshly-`init`'d repository).
///
/// `open_index`'s Err variant is large; boxing it would churn every call site.
#[allow(clippy::result_large_err)]
fn open_or_empty_index(repo: &gix::Repository) -> Result<gix::index::File> {
    if repo.index_path().exists() {
        Ok(repo.open_index()?)
    } else {
        Ok(gix::index::File::from_state(
            gix::index::State::new(repo.object_hash()),
            repo.index_path(),
        ))
    }
}

/// The (path → id, mode) view of an index, used to decide which pathspec-matched
/// paths are modifications and which have vanished from the worktree.
fn tracked_map(index: &gix::index::File) -> HashMap<BString, (ObjectId, Mode)> {
    let backing = index.path_backing();
    index
        .entries()
        .iter()
        .map(|e| (e.path_in(backing).to_owned(), (e.id, e.mode)))
        .collect()
}

/// `git commit --dry-run` (and the `--short`/`--long`/`--porcelain`/`-z` formats
/// that imply it) — a faithful port of `dry_run_commit()` (builtin/commit.c).
///
/// git prepares the index the commit *would* use, points `wt_status` at it, and
/// rolls the preparation back, exiting `0` when something was committable and `1`
/// when nothing was. The report itself comes from the same engine `git status`
/// runs, so the output is identical to `git status` with the matching flags — the
/// prepared index is installed for the duration and the real one put back, which
/// leaves the repository byte-for-byte unchanged just like git's rollback.
fn dry_run_commit(repo: &gix::Repository, o: &DryRun) -> Result<ExitCode> {
    // `-u<mode>` is validated before the report is produced so an invalid mode is
    // a fatal error rather than a status-engine usage message mid-dry-run.
    if let Some(u) = &o.untracked {
        if !matches!(u.as_str(), "no" | "normal" | "all") {
            anyhow::bail!("Invalid untracked files mode '{u}'");
        }
    }

    // The index git would commit from: `-a` stages tracked changes, `-i` adds the
    // named paths to the real index, and a pathspec-limited commit builds the
    // "false index" from HEAD's tree plus those paths.
    let prepared: Option<gix::index::File> = if o.all {
        let mut index = open_or_empty_index(repo)?;
        collect_tracked_changes(repo, &index)?.apply_to(&mut index);
        Some(index)
    } else if o.include {
        let mut index = open_or_empty_index(repo)?;
        include_stage(repo, &o.pathspecs, &index)?.apply_to(&mut index);
        Some(index)
    } else if !o.pathspecs.is_empty() {
        Some(only_mode_stage(repo, &o.pathspecs)?.0)
    } else {
        None
    };

    let committable = index_differs_from_head(repo, prepared.as_ref())?;

    // Translate commit's report flags into the status engine's own spelling.
    let mut sargs: Vec<String> = Vec::new();
    sargs.push(
        match o.format {
            StatusFormat::Short => "--short",
            StatusFormat::Porcelain => "--porcelain",
            StatusFormat::Long | StatusFormat::None => "--long",
        }
        .to_string(),
    );
    if o.null_term {
        sargs.push("-z".to_string());
    }
    if let Some(b) = o.branch_header {
        sargs.push(if b { "--branch" } else { "--no-branch" }.to_string());
    }
    if let Some(ab) = o.ahead_behind {
        sargs.push(if ab { "--ahead-behind" } else { "--no-ahead-behind" }.to_string());
    }
    if let Some(u) = &o.untracked {
        sargs.push(format!("--untracked-files={u}"));
    }

    match &prepared {
        Some(index) => {
            let _swap = IndexSwap::install(repo, index)?;
            super::status::status(&sargs)?;
        }
        None => {
            super::status::status(&sargs)?;
        }
    }

    Ok(if committable {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Whether the given index (or the on-disk one when `None`) records anything
/// different from `HEAD`'s tree — git's `wt_status.committable`, which decides a
/// dry run's exit status. An unmerged entry always counts as committable.
fn index_differs_from_head(
    repo: &gix::Repository,
    index: Option<&gix::index::File>,
) -> Result<bool> {
    let owned;
    let index = match index {
        Some(i) => i,
        None => {
            owned = open_or_empty_index(repo)?;
            &owned
        }
    };
    let flatten = |idx: &gix::index::File| -> Vec<(BString, Option<EntryMode>, ObjectId)> {
        let backing = idx.path_backing();
        idx.entries()
            .iter()
            .map(|e| (e.path_in(backing).to_owned(), e.mode.to_tree_entry_mode(), e.id))
            .collect()
    };
    if index.entries().iter().any(|e| e.stage() != Stage::Unconflicted) {
        return Ok(true);
    }
    let head_tree = match repo.head()?.try_peel_to_id()? {
        Some(id) => Some(repo.find_commit(id.detach())?.tree_id()?.detach()),
        None => None,
    };
    let old = match head_tree {
        Some(t) => flatten(&repo.index_from_tree(&t)?),
        None => Vec::new(),
    };
    Ok(flatten(index) != old)
}

/// Installs a prepared index as the repository's index for the lifetime of the
/// guard, restoring the original on drop — the equivalent of git pointing
/// `the_repository->index_file` at its `next-index-<pid>` file and rolling back.
///
/// The original file is *moved* aside rather than copied, so it comes back with
/// its inode, mode and mtime intact, and the restore runs on every exit path
/// including a panic. `index.lock` is held exclusively for the whole window —
/// the same lock git's own `prepare_index()` takes with `LOCK_DIE_ON_ERROR`, so
/// a concurrent writer (stock git included) cannot walk into the swap.
/// The rollback half of git's `index.lock` around `commit --interactive`.
///
/// git writes the current index into `index.lock`, points `GIT_INDEX_FILE` at
/// the lock, lets the selector stage into *that* copy, and only
/// `commit_index_files()` — reached once the commit object exists and `HEAD` has
/// moved — renames it over the real index. An aborted commit (empty message,
/// failing `pre-commit`, an editor that exits non-zero) instead rolls the lock
/// back and the selection is discarded.
///
/// This build's index plumbing ignores `GIT_INDEX_FILE`, so the `apply --cached`
/// child would write the real index whatever the environment said. The selector
/// therefore runs against the real index and the *original* bytes are held here
/// instead, restored by [`Drop`] unless [`Self::keep`] has been called. Both
/// end states — kept on success, discarded on abort — are git's.
struct InteractiveStage {
    /// The repository index the selector stages into.
    index: std::path::PathBuf,
    /// The index as it was before the selector ran, or `None` when the
    /// repository had no index file at all.
    original: Option<Vec<u8>>,
    /// Set once the commit has succeeded, which disarms the rollback.
    keep: bool,
}

impl InteractiveStage {
    fn hold(repo: &gix::Repository) -> Result<Self> {
        let index = repo.index_path();
        let original = match std::fs::read(&index) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Ok(Self { index, original, keep: false })
    }

    /// git's `commit_index_files()`: the staged selection stands.
    fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for InteractiveStage {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        match &self.original {
            Some(bytes) => {
                let _ = std::fs::write(&self.index, bytes);
            }
            None => {
                let _ = std::fs::remove_file(&self.index);
            }
        }
    }
}

struct IndexSwap {
    /// The repository index path the prepared index was written to.
    index: std::path::PathBuf,
    /// Where the original was moved, or `None` when there was no index file.
    backup: Option<std::path::PathBuf>,
    /// The `index.lock` this guard created and must remove.
    lock: std::path::PathBuf,
}

impl IndexSwap {
    /// Take `index.lock`, move the real index aside and write `prepared` in its
    /// place. Fails while another process holds the lock, exactly as git does.
    fn install(repo: &gix::Repository, prepared: &gix::index::File) -> Result<Self> {
        let index = repo.index_path();
        let lock = index.with_file_name("index.lock");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(|e| {
                anyhow::anyhow!(
                    "Unable to create '{}': {e}\n\n\
                     Another git process seems to be running in this repository.",
                    lock.display()
                )
            })?;
        // From here on the guard owns the lock, so every failure path removes it.
        let mut guard = IndexSwap { index, backup: None, lock };
        if guard.index.exists() {
            let backup = guard.index.with_file_name("index.zvcs-dry-run");
            std::fs::rename(&guard.index, &backup)?;
            guard.backup = Some(backup);
        }
        let mut bytes = Vec::new();
        prepared.write_to(&mut bytes, gix::index::write::Options::default())?;
        std::fs::write(&guard.index, &bytes)?;
        Ok(guard)
    }
}

impl Drop for IndexSwap {
    fn drop(&mut self) {
        match &self.backup {
            Some(b) => {
                let _ = std::fs::rename(b, &self.index);
            }
            None => {
                let _ = std::fs::remove_file(&self.index);
            }
        }
        let _ = std::fs::remove_file(&self.lock);
    }
}

/// The commented block git puts below the message in the editor buffer: the
/// cleanup-mode-specific hint (or, for `scissors`, the cut line) followed by a
/// minimal status header.
///
/// The hint wording is git's, chosen by cleanup mode in `prepare_to_commit()`.
/// The status body is a reduced form of `wt_status_print()` — the branch line and
/// the initial-commit marker — not the full staged/unstaged/untracked listing.
fn editor_status_block(
    repo: &gix::Repository,
    is_root: bool,
    comment: &str,
    cleanup: Cleanup,
    whence: Whence,
) -> Result<String> {
    let mut buf = String::new();
    // `prepare_to_commit()` warns above everything else when an operation is being
    // concluded, and moves the scissors line above the warning with it so the
    // warning survives a `--cleanup=scissors` message.
    if whence != Whence::Commit {
        if cleanup == Cleanup::Scissors {
            buf.push_str(&scissors_line(comment));
        }
        let (what, refname) = match whence {
            Whence::Merge => ("merge", "MERGE_HEAD"),
            _ => ("cherry-pick", "CHERRY_PICK_HEAD"),
        };
        // `status_printf_ln()` comments each line, indents nothing after a leading
        // tab, and its `trail` adds the blank line before git's own `fprintf("\n")`.
        buf.push_str(&format!(
            "{comment}\n\
             {comment} It looks like you may be committing a {what}.\n\
             {comment} If this is not correct, please run\n\
             {comment}\tgit update-ref -d {refname}\n\
             {comment} and try again.\n\
             \n"
        ));
    }
    buf.push('\n');
    match cleanup {
        Cleanup::Strip => {
            buf.push_str(&format!(
                "{comment} Please enter the commit message for your changes. Lines starting\n"
            ));
            buf.push_str(&format!(
                "{comment} with '{comment}' will be ignored, and an empty message aborts the commit.\n"
            ));
        }
        // Already emitted above when an operation is being concluded.
        Cleanup::Scissors if whence == Whence::Commit => buf.push_str(&scissors_line(comment)),
        Cleanup::Scissors => {}
        Cleanup::Whitespace | Cleanup::Verbatim => {
            buf.push_str(&format!(
                "{comment} Please enter the commit message for your changes. Lines starting\n"
            ));
            buf.push_str(&format!(
                "{comment} with '{comment}' will be kept; you may remove them yourself if you want to.\n"
            ));
            buf.push_str(&format!(
                "{comment} An empty message aborts the commit.\n"
            ));
        }
    }
    buf.push_str(&format!("{comment}\n"));
    match repo.head_name()? {
        Some(b) => buf.push_str(&format!("{comment} On branch {}\n", b.shorten())),
        None => buf.push_str(&format!("{comment} HEAD detached\n")),
    }
    if is_root {
        buf.push_str(&format!("{comment}\n{comment} Initial commit\n"));
    }
    buf.push_str(&format!("{comment}\n"));
    Ok(buf)
}

/// git's `wt_status_add_cut_line()`: the `>8` scissors line plus the two-line
/// explanation, each commented with the configured prefix.
fn scissors_line(comment: &str) -> String {
    format!(
        "{comment} ------------------------ >8 ------------------------\n\
         {comment} Do not modify or remove the line above.\n\
         {comment} Everything below it will be ignored.\n"
    )
}

/// `-v`/`--verbose`: append the staged diff below a cut line so the editor shows
/// what is about to be committed. git renders it in-process; we run this very
/// binary's `diff --cached`, whose output is the same, straight into the buffer.
/// The message is truncated at the cut line afterward, so the diff never lands in
/// the commit.
fn append_verbose_diff(
    repo: &gix::Repository,
    msg_path: &std::path::Path,
    cleanup: Cleanup,
) -> Result<()> {
    use std::io::Write as _;
    let comment = comment_prefix(&repo.config_snapshot());
    let mut file = std::fs::OpenOptions::new().append(true).open(msg_path)?;
    // `--cleanup=scissors` already put the cut line above the status block, and
    // git never writes a second one.
    if cleanup != Cleanup::Scissors {
        file.write_all(scissors_line(&comment).as_bytes())?;
    }
    file.flush()?;
    let exe = std::env::current_exe()?;
    let workdir = repo.workdir().unwrap_or_else(|| repo.git_dir()).to_owned();
    let _ = std::process::Command::new(exe)
        .args(["diff", "--cached"])
        .current_dir(&workdir)
        .stdout(file)
        .stderr(std::process::Stdio::null())
        .status();
    Ok(())
}

/// `--trailer <token>[(=|:)<value>]` — git spawns
/// `git interpret-trailers --in-place --no-divider <COMMIT_EDITMSG> <--trailer v>…`
/// and we call that exact implementation, with that exact argument order.
fn apply_trailers(msg_path: &std::path::Path, trailers: &[String]) -> Result<()> {
    let mut args: Vec<String> = vec![
        "--in-place".to_string(),
        "--no-divider".to_string(),
        msg_path.to_string_lossy().into_owned(),
    ];
    for t in trailers {
        args.push("--trailer".to_string());
        args.push(t.clone());
    }
    super::interpret_trailers::interpret_trailers(&args)?;
    Ok(())
}

/// Port of `template_untouched()` (builtin/commit.c): true when the cleaned-up
/// message is the cleaned-up template with nothing but blanks and comments added,
/// which aborts the commit. `verbatim` cleanup exempts a non-empty message.
fn template_untouched(message: &str, template: &str, cleanup: Cleanup, comment: &str) -> bool {
    if cleanup == Cleanup::Verbatim && !message.is_empty() {
        return false;
    }
    let rest = message.strip_prefix(template).unwrap_or(message);
    // `rest_is_empty()`: only whitespace and comment lines may follow.
    rest.lines()
        .all(|l| l.trim().is_empty() || l.starts_with(comment))
}

/// Resolve `--cleanup=<mode>` (else `commit.cleanup`) into git's
/// `commit_msg_cleanup_mode` — a port of `get_cleanup_mode()`, whose `default`
/// and `scissors` answers both depend on whether an editor is used.
fn resolve_cleanup(
    arg: Option<&str>,
    snap: &gix::config::Snapshot<'_>,
    use_editor: bool,
) -> Result<Cleanup> {
    let configured = snap.string("commit.cleanup").map(|v| v.to_string());
    let mode = arg.or(configured.as_deref());
    Ok(match mode {
        None | Some("default") => {
            if use_editor {
                Cleanup::Strip
            } else {
                Cleanup::Whitespace
            }
        }
        Some("verbatim") => Cleanup::Verbatim,
        Some("whitespace") => Cleanup::Whitespace,
        Some("strip") => Cleanup::Strip,
        Some("scissors") => {
            if use_editor {
                Cleanup::Scissors
            } else {
                Cleanup::Whitespace
            }
        }
        Some(other) => anyhow::bail!("Invalid cleanup mode {other}"),
    })
}

/// Write the commit object, optionally carrying a `gpgsig` header.
///
/// git signs the *unsigned* serialization and then inserts the armored signature
/// as an extra header, which is exactly what happens here: the object is encoded
/// once without the header, handed to `gpg -bsa`, and re-encoded with `gpgsig`
/// first among the extra headers — the slot git writes it in.
fn write_commit_object(
    repo: &gix::Repository,
    committer: &gix::actor::Signature,
    author: &gix::actor::Signature,
    message: &str,
    tree: ObjectId,
    parents: Vec<ObjectId>,
    signer: Option<&Signer>,
) -> Result<ObjectId> {
    let mut commit = gix::objs::Commit {
        tree,
        parents: parents.into(),
        author: author.clone(),
        committer: committer.clone(),
        encoding: None,
        message: message.into(),
        extra_headers: Vec::new(),
    };
    if let Some(s) = signer {
        let mut payload = Vec::new();
        gix::objs::WriteTo::write_to(&commit, &mut payload)?;
        let sig = crate::gitsig::sign(&payload, &s.program, s.key.as_deref()).map_err(|e| {
            eprintln!("error: gpg failed to sign the data:\n{e}");
            anyhow::anyhow!("failed to write commit object")
        })?;
        commit.extra_headers.push(("gpgsig".into(), sig.into()));
    }
    Ok(repo.write_object(&commit)?.detach())
}

/// `git commit <pathspec>...` — git's default `--only`/`-o` mode.
///
/// The commit tree is HEAD's tree with only the matched pathspec paths replaced
/// by their WORKING-TREE content: a present file is added/modified, a tracked
/// path whose worktree file is gone is deleted, and every other path keeps its
/// HEAD version — so any staged (index) changes to *other* paths are disregarded.
/// After the tree is built the same matched paths are staged into the real
/// on-disk index (leaving unrelated index entries untouched) so later commits see
/// them. Returns `(tree_id, new_entries)` for the caller's summary/short-stat.
///
/// A pathspec matches a path when the path equals it or lives under `<spec>/`;
/// literal files and directory prefixes are supported, as are the worktree globs
/// the dirwalk resolves. Blob hashing and mode detection mirror `git add`.
/// A staged-entry snapshot for the commit summary: (repo-relative path, mode, id).
type StagedEntry = (BString, EntryMode, ObjectId);

fn build_only_mode_tree(
    repo: &gix::Repository,
    pathspecs: &[String],
) -> Result<(ObjectId, Vec<StagedEntry>)> {
    // The commit tree comes from git's "false index" — HEAD's tree with only the
    // matched paths taken from the worktree. The same staged set is then applied
    // to the real index, so the worktree is walked and hashed exactly once.
    let (temp, staged) = only_mode_stage(repo, pathspecs)?;

    let hash = repo.object_hash();
    let mut editor = gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, hash);
    let mut new_entries: Vec<(BString, EntryMode, ObjectId)> = Vec::new();
    {
        let backing = temp.path_backing();
        new_entries.reserve(temp.entries().len());
        for entry in temp.entries() {
            let path = entry.path_in(backing);
            let mode = entry
                .mode
                .to_tree_entry_mode()
                .ok_or_else(|| anyhow::anyhow!("index entry `{path}` has an unrepresentable mode"))?;
            editor.upsert(
                path.split(|&b| b == b'/').map(|c| c.as_bstr()),
                mode.kind(),
                entry.id,
            )?;
            new_entries.push((path.to_owned(), mode, entry.id));
        }
    }
    let tree_id = editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?;

    // Stage the same paths into the REAL on-disk index, leaving all other entries
    // — git's step (2)/(3), which is what makes the partial commit visible to the
    // next one.
    let mut real = open_or_empty_index(repo)?;
    staged.apply_to(&mut real);
    real.write(gix::index::write::Options::default())?;

    Ok((tree_id, new_entries))
}

/// `-i`/`--include <paths>`: refresh the *index-known* paths from the worktree,
/// which is what `add_files_to_cache()` does for git's `also` mode. Untracked
/// paths are not added — a pathspec that matches none of the index is fatal.
fn include_stage(
    repo: &gix::Repository,
    pathspecs: &[String],
    index: &gix::index::File,
) -> Result<StagedSet> {
    let tracked = tracked_map(index);
    let known: HashSet<BString> = tracked.keys().cloned().collect();
    stage_pathspecs(repo, pathspecs, &tracked, &known)
}

/// HEAD's tree id, refusing an unborn branch the way a pathspec-limited commit
/// must (it has no base tree to build upon).
fn head_tree(repo: &gix::Repository) -> Result<ObjectId> {
    let head_commit = repo
        .head()?
        .try_peel_to_id()?
        .ok_or_else(|| {
            anyhow::anyhow!("cannot do a pathspec-limited commit on an unborn branch (no HEAD)")
        })?
        .detach();
    Ok(repo.find_commit(head_commit)?.tree_id()?.detach())
}

/// git's "false index" for a partial commit: HEAD's tree with only the matched
/// pathspec paths replaced by their worktree content. Everything else keeps its
/// HEAD version, so staged changes to other paths are disregarded.
///
/// The pathspecs are matched against git's `overlay_tree_on_index` view — the
/// real index unioned with HEAD's tree — so a path that is staged but not yet in
/// HEAD counts, while a wholly untracked one does not. The staged set is returned
/// alongside so the caller can replay it onto the real index without re-hashing.
fn only_mode_stage(
    repo: &gix::Repository,
    pathspecs: &[String],
) -> Result<(gix::index::File, StagedSet)> {
    let head_tree_id = head_tree(repo)?;
    let mut temp = repo.index_from_tree(&head_tree_id)?;
    let tracked = tracked_map(&temp);
    let mut known: HashSet<BString> = tracked.keys().cloned().collect();
    let real = open_or_empty_index(repo)?;
    let backing = real.path_backing();
    known.extend(real.entries().iter().map(|e| e.path_in(backing).to_owned()));
    let staged = stage_pathspecs(repo, pathspecs, &tracked, &known)?;
    staged.apply_to(&mut temp);
    Ok((temp, staged))
}

/// A worktree file to write into an index: the blob that was hashed for it, its
/// mode and the stat data that lets a later `git status` skip re-reading it.
struct StagedFile {
    /// Repo-relative path.
    path: BString,
    /// The blob id written for the worktree content.
    id: ObjectId,
    /// The index mode derived from the file (regular, executable, symlink).
    mode: Mode,
    /// The worktree stat data recorded alongside the entry.
    stat: Stat,
}

/// The outcome of matching pathspecs (or, for `-a`, every tracked path) against
/// the worktree: entries to (re)write and paths that vanished and must go.
struct StagedSet {
    /// Paths whose worktree content was hashed into the object database.
    staged: Vec<StagedFile>,
    /// Tracked paths whose worktree file is gone.
    deletions: Vec<BString>,
}

impl StagedSet {
    /// Nothing matched — used to skip an index write entirely.
    fn is_empty(&self) -> bool {
        self.staged.is_empty() && self.deletions.is_empty()
    }

    /// Replace every touched path in `index` wholesale, then restore sort order.
    /// The tree-cache extension is dropped so a later tree build cannot pick up a
    /// stale subtree for a path that just moved.
    fn apply_to(&self, index: &mut gix::index::File) {
        let remove: HashSet<BString> = self
            .staged
            .iter()
            .map(|s| s.path.clone())
            .chain(self.deletions.iter().cloned())
            .collect();
        index.remove_entries(|_, path, _| remove.contains(&path.to_owned()));
        for s in &self.staged {
            index.dangerously_push_entry(s.stat, s.id, Flags::empty(), s.mode, s.path.as_ref());
        }
        index.sort_entries();
        index.remove_tree();
    }
}

/// Hash the worktree content of every path matching `pathspecs`, and collect the
/// tracked paths the pathspecs match whose worktree file has vanished.
///
/// `tracked` is the base the deletion decision is taken against — HEAD's tree for
/// a partial (`--only`) commit, the real index for `-i`/`--include`. `known` is
/// the set of paths git will consider at all: only-mode uses the index overlaid
/// with HEAD, `--include` the index alone, and a pathspec matching nothing in it
/// is the fatal `did not match any file(s) known to git`. So neither mode ever
/// picks up a wholly untracked file, exactly as git's `list_paths()` and
/// `add_files_to_cache()` refuse to.
///
/// A pathspec matches a path when the path equals it or lives under `<spec>/`;
/// literal files and directory prefixes are supported, as are the worktree globs
/// the dirwalk resolves. Blob hashing and mode detection mirror `git add`.
fn stage_pathspecs(
    repo: &gix::Repository,
    pathspecs: &[String],
    tracked: &HashMap<BString, (ObjectId, Mode)>,
    known: &HashSet<BString>,
) -> Result<StagedSet> {
    if repo.workdir().is_none() {
        anyhow::bail!("this operation must be run in a work tree");
    }

    // Walk the worktree for files matching the pathspecs (mirrors `git add`).
    let patterns: Vec<BString> = pathspecs
        .iter()
        .map(|s| BString::from(s.clone().into_bytes()))
        .collect();
    let options = repo
        .dirwalk_options()?
        .emit_tracked(true)
        .emit_ignored(Some(gix::dir::walk::EmissionMode::Matching));
    let dirwalk_index = repo.index_or_load_from_head_or_empty()?;
    let mut iter = repo.dirwalk_iter(dirwalk_index, patterns, Default::default(), options)?;

    let mut staged: Vec<StagedFile> = Vec::new();
    let mut staged_set: HashSet<BString> = HashSet::new();

    for item in iter.by_ref() {
        let entry = item?.entry;
        // Only regular files and symlinks carry stageable content.
        match entry.disk_kind {
            Some(gix::dir::entry::Kind::File) | Some(gix::dir::entry::Kind::Symlink) => {}
            _ => continue,
        }
        let path = entry.rela_path;
        // git only ever updates paths it already knows: `git commit <untracked>`
        // and `git commit -i <untracked>` both fail rather than adding the file.
        if !known.contains(&path) {
            continue;
        }
        let Some(abs) = repo.workdir_path(&path) else {
            continue;
        };
        let md = gix::index::fs::Metadata::from_path_no_follow(&abs)?;
        // A tracked path replaced by a directory is not stageable content.
        if md.is_dir() {
            continue;
        }
        let (bytes, mode) = if md.is_symlink() {
            let target = std::fs::read_link(&abs)?;
            #[cfg(unix)]
            let bytes = {
                use std::os::unix::ffi::OsStrExt;
                target.as_os_str().as_bytes().to_vec()
            };
            #[cfg(not(unix))]
            let bytes = target.to_string_lossy().into_owned().into_bytes();
            (bytes, Mode::SYMLINK)
        } else {
            let bytes = std::fs::read(&abs)?;
            let mode = if md.is_executable() {
                Mode::FILE_EXECUTABLE
            } else {
                Mode::FILE
            };
            (bytes, mode)
        };
        let id = repo.write_blob(&bytes)?.detach();
        staged_set.insert(path.clone());
        staged.push(StagedFile { path, id, mode, stat: Stat::from_fs(&md)? });
    }

    // Recover the pathspec matcher (used to decide deletions) from the walk.
    let mut pathspec = match iter.into_outcome() {
        Some(outcome) => outcome.pathspec,
        None => anyhow::bail!("directory walk did not complete"),
    };

    // Deletions: tracked paths matched by the pathspec whose worktree file is gone.
    let mut deletions: Vec<BString> = Vec::new();
    for path in tracked.keys() {
        if staged_set.contains(path) || !pathspec.is_included(path.as_bstr(), Some(false)) {
            continue;
        }
        let gone = match repo.workdir_path(path.as_bstr()) {
            Some(p) => std::fs::symlink_metadata(p).is_err(),
            None => true,
        };
        if gone {
            deletions.push(path.clone());
        }
    }

    // Each explicit (non-magic, non-glob) pathspec must match a path git already
    // knows — `report_path_error()`'s `did not match any file(s) known to git`. A
    // known path that is present but unchanged still counts (its entry is simply
    // left alone), which is why the whole `known` set is searched, not just the
    // paths that were restaged.
    for p in pathspecs {
        if p == "." || p.starts_with(':') || p.contains(['*', '?', '[']) {
            continue;
        }
        let pb = p.as_bytes();
        let mut prefix = pb.to_vec();
        prefix.push(b'/');
        let matched = known
            .iter()
            .any(|x| x.as_slice() == pb || x.as_slice().starts_with(&prefix));
        if !matched {
            anyhow::bail!("pathspec '{p}' did not match any file(s) known to git");
        }
    }

    Ok(StagedSet { staged, deletions })
}

/// Stage every *tracked* path whose worktree state diverges from the index —
/// `git commit -a`, which is `git add -u` over the whole worktree.
///
/// Only stage-0 entries participate: conflicted stages are left for the caller's
/// unmerged-files check to reject, and submodule gitlinks are never re-read from
/// the worktree here. Untracked files are deliberately not added, which is the
/// whole distinction between `-a` and `git add -A`.
///
/// Content filters (`autocrlf`, `clean`/`smudge`) are not applied, matching the
/// same deviation `git add` carries in this port.
fn stage_tracked_changes(repo: &gix::Repository) -> Result<()> {
    if !repo.index_path().exists() {
        return Ok(());
    }
    let mut index = repo.open_index()?;
    let staged = collect_tracked_changes(repo, &index)?;
    if staged.is_empty() {
        return Ok(());
    }
    staged.apply_to(&mut index);
    index.write(gix::index::write::Options::default())?;
    Ok(())
}

/// The `-a`/`--all` scan itself: every stage-0, non-gitlink index entry whose
/// worktree content or mode moved, plus the tracked paths that vanished.
/// Split out from [`stage_tracked_changes`] so `--dry-run -a` can build the
/// prepared index without writing it.
fn collect_tracked_changes(
    repo: &gix::Repository,
    index: &gix::index::File,
) -> Result<StagedSet> {
    if repo.workdir().is_none() {
        anyhow::bail!("this operation must be run in a work tree");
    }
    let mut staged: Vec<StagedFile> = Vec::new();
    let mut deletions: Vec<BString> = Vec::new();

    {
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted || e.mode == Mode::COMMIT {
                continue;
            }
            let path = e.path_in(backing).to_owned();
            let Some(abs) = repo.workdir_path(&path) else {
                continue;
            };
            // A vanished (or unreadable) tracked path stages as a deletion.
            let Ok(md) = gix::index::fs::Metadata::from_path_no_follow(&abs) else {
                deletions.push(path);
                continue;
            };
            // A tracked file replaced by a directory is not stageable content;
            // leave the index entry untouched rather than guessing.
            if md.is_dir() {
                continue;
            }

            let (bytes, mode) = if md.is_symlink() {
                let target = std::fs::read_link(&abs)?;
                #[cfg(unix)]
                let bytes = {
                    use std::os::unix::ffi::OsStrExt;
                    target.as_os_str().as_bytes().to_vec()
                };
                #[cfg(not(unix))]
                let bytes = target.to_string_lossy().into_owned().into_bytes();
                (bytes, Mode::SYMLINK)
            } else {
                let bytes = std::fs::read(&abs)?;
                let mode = if md.is_executable() {
                    Mode::FILE_EXECUTABLE
                } else {
                    Mode::FILE
                };
                (bytes, mode)
            };

            // Hash first, write only on a real change: an unmodified worktree
            // must not churn the index or touch the object database.
            let id = gix::objs::compute_hash(repo.object_hash(), gix::object::Kind::Blob, &bytes)?;
            if id == e.id && mode == e.mode {
                continue;
            }
            let id = repo.write_blob(&bytes)?.detach();
            staged.push(StagedFile {
                path,
                id,
                mode,
                stat: Stat::from_fs(&md)?,
            });
        }
    }

    Ok(StagedSet { staged, deletions })
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

/// git's editor path for `git commit` without `-m`: build a template from
/// `commit.template` and a commented status header, open it in the configured
/// editor, and return the cleaned-up message per `commit.cleanup`.
/// Parse a `--author` value of the form `Name <email>` into (name, email),
/// splitting on the last `<`…`>` as git's `split_ident_line` does. git also
/// accepts a bare string that searches existing commits' authors; that lookup
/// form is not ported.
fn parse_author_ident(s: &str) -> Result<(String, String)> {
    match (s.rfind('<'), s.rfind('>')) {
        (Some(o), Some(c)) if c > o => Ok((s[..o].trim().to_string(), s[o + 1..c].to_string())),
        _ => anyhow::bail!(
            "--author '{s}': only the `Name <email>` form is supported (author search is not ported)"
        ),
    }
}

/// The comment prefix for message templates: `core.commentString` (a multi-byte
/// prefix, git 2.45+) if set, else `core.commentChar` (a single character),
/// defaulting to `#`. `auto` is treated as the default here.
fn comment_prefix(snap: &gix::config::Snapshot<'_>) -> String {
    if let Some(v) = snap.string("core.commentString") {
        let v = v.to_string();
        if !v.is_empty() && v != "auto" {
            return v;
        }
    }
    match snap.string("core.commentChar") {
        None => "#".to_string(),
        Some(v) => {
            let s = v.to_string();
            if s.is_empty() || s == "auto" {
                "#".to_string()
            } else {
                // core.commentChar is a single character.
                s.chars().next().unwrap_or('#').to_string()
            }
        }
    }
}

/// Resolve the editor command git would use: `GIT_EDITOR` → `core.editor` →
/// `$VISUAL` → `$EDITOR`, else `vi`. On a dumb/non-interactive terminal with no
/// editor configured, git refuses rather than launching a broken editor.
fn resolve_editor(snap: &gix::config::Snapshot<'_>) -> Result<String> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if let Some(e) = env("GIT_EDITOR") {
        return Ok(e);
    }
    if let Some(e) = snap.string("core.editor") {
        return Ok(e.to_string());
    }
    if let Some(e) = env("VISUAL") {
        return Ok(e);
    }
    if let Some(e) = env("EDITOR") {
        return Ok(e);
    }
    let dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(true);
    if dumb || !std::io::stdin().is_terminal() {
        anyhow::bail!("Terminal is dumb, but EDITOR unset. Please supply the message using -m.");
    }
    Ok("vi".to_string())
}

/// Open `path` in the configured editor and wait, git-style: the editor string
/// runs through the shell so `core.editor = "code -w"` and other argument-bearing
/// commands work, and stdio is inherited so the interactive editor owns the tty.
fn launch_editor(snap: &gix::config::Snapshot<'_>, path: &std::path::Path) -> Result<()> {
    let editor = resolve_editor(snap)?;
    // `launch_specified_editor` (editor.c): when stderr is a terminal and
    // `advice.waitingForEditor` is on, git says why it is blocked before handing
    // the tty over. A dumb terminal cannot erase the line afterwards, so it gets
    // a newline instead of the erase sequence. The hint is never printed when
    // stderr is redirected, which is why scripted runs see none of this.
    let waiting = std::io::IsTerminal::is_terminal(&std::io::stderr())
        && crate::advice::Advice::WaitingForEditor.enabled();
    let dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(true);
    if waiting {
        use std::io::Write;
        let tail = if dumb { "\n" } else { " " };
        eprint!("hint: Waiting for your editor to close the file...{tail}");
        let _ = std::io::stderr().flush();
    }
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(&editor) // $0
        .arg(path) // $1
        .status()
        .map_err(|e| anyhow::anyhow!("cannot run editor '{editor}': {e}"))?;
    // `term_clear_line()`: wipe the "Waiting for your editor" line so the
    // command's real output starts on a clean line.
    if waiting && !dumb {
        use std::io::Write;
        eprint!("\r\x1b[K");
        let _ = std::io::stderr().flush();
    }
    if !status.success() {
        anyhow::bail!("there was a problem with the editor '{editor}'");
    }
    Ok(())
}

/// git's `commit_msg_cleanup_mode` (builtin/commit.c), resolved by
/// [`resolve_cleanup`] from `--cleanup`/`commit.cleanup` and whether an editor
/// is used.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cleanup {
    /// `strip` (`COMMIT_MSG_CLEANUP_ALL`) — whitespace cleanup plus comment lines.
    Strip,
    /// `whitespace` (`COMMIT_MSG_CLEANUP_SPACE`).
    Whitespace,
    /// `verbatim` (`COMMIT_MSG_CLEANUP_NONE`) — the message is recorded as typed.
    Verbatim,
    /// `scissors` — whitespace cleanup after truncating at the `>8` cut line.
    Scissors,
}

/// Apply git's `cleanup_message()`: `scissors` (and any `-v`/`--verbose` run)
/// first truncates the buffer at the `>8` cut line, then `verbatim` leaves the
/// text untouched while the others trim trailing whitespace, collapse runs of
/// blank lines and drop leading/trailing blank lines. `strip` additionally
/// removes lines beginning with the comment prefix.
fn cleanup_message(raw: &str, comment: &str, mode: Cleanup, verbose: bool) -> String {
    // `strbuf_setlen(msg, wt_status_locate_end(...))` — the cut line and
    // everything below it never reach the commit.
    let raw = if verbose || mode == Cleanup::Scissors {
        &raw[..wt_status_locate_end(raw.as_bytes(), comment)]
    } else {
        raw
    };
    if let Cleanup::Verbatim = mode {
        return raw.to_string();
    }
    let strip_comments = matches!(mode, Cleanup::Strip);

    let mut out: Vec<&str> = Vec::new();
    let mut prev_blank = true; // drop leading blank lines
    for line in raw.lines() {
        if strip_comments && line.starts_with(comment) {
            continue;
        }
        let line = line.trim_end();
        let blank = line.is_empty();
        if blank && prev_blank {
            continue;
        }
        out.push(line);
        prev_blank = blank;
    }
    while out.last() == Some(&"") {
        out.pop();
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// Expand a leading `~`/`~/` to `$HOME`, as git does for path-valued config.
fn expand_tilde(tok: &str) -> std::path::PathBuf {
    if tok == "~" {
        if let Some(h) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(h);
        }
    } else if let Some(rest) = tok.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(h).join(rest);
        }
    }
    std::path::PathBuf::from(tok)
}

/// git's folded `%s` subject: skip leading blank lines, then join the lines of
/// the first paragraph (each right-trimmed) with a single space, stopping at the
/// first blank line — `format_subject()` in pretty.c with a `" "` separator.
fn folded_subject(msg: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut started = false;
    for line in msg.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            if started {
                break;
            }
            continue;
        }
        started = true;
        out.push(line);
    }
    out.join(" ")
}

/// The first non-blank line of a commit message — git's raw subject start, used
/// to detect an existing `amend!` subject in `prepare_amend_commit()`.
fn subject_line(msg: &str) -> &str {
    msg.lines().find(|l| !l.trim_end().is_empty()).unwrap_or("")
}

/// git's `%b`: the message with its subject paragraph and the blank line(s)
/// terminating it removed, leaving the body verbatim.
fn message_body(msg: &str) -> String {
    let b = msg.as_bytes();
    let n = b.len();
    let line_end = |i: usize| {
        b[i..]
            .iter()
            .position(|&c| c == b'\n')
            .map(|p| i + p + 1)
            .unwrap_or(n)
    };
    let blank = |i: usize, e: usize| b[i..e].iter().all(|&c| matches!(c, b'\n' | b'\r' | b' ' | b'\t'));
    let mut i = 0usize;
    // leading blank lines, the subject paragraph, then its trailing blank lines.
    while i < n {
        let e = line_end(i);
        if blank(i, e) {
            i = e;
        } else {
            break;
        }
    }
    while i < n {
        let e = line_end(i);
        if blank(i, e) {
            break;
        }
        i = e;
    }
    while i < n {
        let e = line_end(i);
        if blank(i, e) {
            i = e;
        } else {
            break;
        }
    }
    msg[i..].to_string()
}

/// `-s`/`--signoff`: append a `Signed-off-by: <ident>` trailer, a faithful port
/// of `append_signoff()` (sequencer.c) with `flag == 0` (no dedup). The trailer
/// is merged into an existing trailer block, or set off by a blank line after a
/// message body, and is skipped only when it is already the last trailer.
fn append_signoff(msg: &mut String, ident: &str) {
    let sob = format!("Signed-off-by: {ident}\n");
    let ignore_footer = ignore_non_trailer(msg.as_bytes());
    // strbuf_complete_line: only when there is no trailing footer to preserve.
    if ignore_footer == 0 && !msg.is_empty() && !msg.ends_with('\n') {
        msg.push('\n');
    }
    let cut = msg.len() - ignore_footer;
    let sob_bytes = sob.as_bytes();
    // If the whole (footer-stripped) buffer equals the sob, treat it as present.
    let has_footer: u8 = if cut == sob_bytes.len() && &msg.as_bytes()[..cut] == sob_bytes {
        3
    } else {
        has_conforming_footer(&msg.as_bytes()[..cut], sob_bytes)
    };
    if has_footer == 0 {
        // Leave a blank line between a message body and the sob.
        // Distinct cases mirror git C source; the `cut == 1` arm also guards the
        // `cut - 2` index below from underflowing, so keep them separate.
        #[allow(clippy::if_same_then_else)]
        let append = if cut == 0 {
            Some("\n\n")
        } else if cut == 1 {
            Some("\n")
        } else if msg.as_bytes()[cut - 2] != b'\n' {
            Some("\n")
        } else {
            None
        };
        if let Some(a) = append {
            let pos = msg.len() - ignore_footer;
            msg.insert_str(pos, a);
        }
    }
    if has_footer != 3 {
        let pos = msg.len() - ignore_footer;
        msg.insert_str(pos, &sob);
    }
}

/// Port of `has_conforming_footer()` (sequencer.c) for the default `flag == 0`
/// path: returns `0` when the tail has no trailer block, `3` when `sob` is the
/// last trailer, `2` when `sob` appears earlier, `1` otherwise. `sub` is the
/// message truncated to `len - ignore_footer`.
fn has_conforming_footer(sub: &[u8], sob: &[u8]) -> u8 {
    let start = find_trailer_start(sub);
    let end = sub.len() - ignore_non_trailer(sub);
    if start >= end {
        return 0;
    }
    // Trailer starts are the non-comment, non-continuation, non-blank lines.
    let mut trailer_starts: Vec<usize> = Vec::new();
    let mut off = start;
    while off < end {
        let e = next_line_off(sub, off);
        let line = &sub[off..e];
        let is_comment = line.first() == Some(&b'#');
        let is_cont = matches!(line.first(), Some(&b' ') | Some(&b'\t'));
        if !is_comment && !is_cont && !sig_is_blank_line(&sub[off..]) {
            trailer_starts.push(off);
        }
        off = e;
    }
    let last_idx = trailer_starts.len().wrapping_sub(1);
    let mut found_sob = false;
    let mut found_sob_last = false;
    for (k, &o) in trailer_starts.iter().enumerate() {
        if sub[o..].starts_with(sob) {
            found_sob = true;
            if k == last_idx {
                found_sob_last = true;
            }
        }
    }
    if found_sob_last {
        3
    } else if found_sob {
        2
    } else {
        1
    }
}

/// Port of `find_trailer_start()` (trailer.c) for the default configuration
/// (separator `:`, comment char `#`, no configured trailer tokens). Returns the
/// offset of the first trailer line, or `buf.len()` when there is no trailer
/// block. `recognized_prefix` is set only by the git-generated prefixes below,
/// since the user's trailer config is empty in this port's signoff path.
fn find_trailer_start(buf: &[u8]) -> usize {
    let len = buf.len();
    const GEN: [&[u8]; 2] = [b"Signed-off-by: ", b"(cherry picked from commit "];

    // The first paragraph is the title and cannot hold trailers.
    let mut s = 0usize;
    while s < len {
        if buf[s] == b'#' {
            s = next_line_off(buf, s);
            continue;
        }
        if sig_is_blank_line(&buf[s..]) {
            break;
        }
        s = next_line_off(buf, s);
    }
    let end_of_title = s as isize;

    let mut only_spaces = true;
    let mut recognized_prefix = false;
    let mut trailer_lines: i64 = 0;
    let mut non_trailer_lines: i64 = 0;
    let mut possible_continuation_lines: i64 = 0;

    let mut l = last_line(buf, len);
    while l >= end_of_title {
        let bol = l as usize;
        let line = &buf[bol..];
        if line.first() == Some(&b'#') {
            non_trailer_lines += possible_continuation_lines;
            possible_continuation_lines = 0;
            l = last_line(buf, bol);
            continue;
        }
        if sig_is_blank_line(line) {
            if only_spaces {
                l = last_line(buf, bol);
                continue;
            }
            non_trailer_lines += possible_continuation_lines;
            // Distinct conditions mirror git C source; merging obscures the port.
            #[allow(clippy::if_same_then_else)]
            if recognized_prefix && trailer_lines * 3 >= non_trailer_lines {
                return next_line_off(buf, bol);
            } else if trailer_lines > 0 && non_trailer_lines == 0 {
                return next_line_off(buf, bol);
            }
            return len;
        }
        only_spaces = false;

        let mut matched_gen = false;
        for g in GEN.iter() {
            if line.starts_with(g) {
                trailer_lines += 1;
                possible_continuation_lines = 0;
                recognized_prefix = true;
                matched_gen = true;
                break;
            }
        }
        if matched_gen {
            l = last_line(buf, bol);
            continue;
        }

        let sep = find_separator(line);
        if sep >= 1 && !line[0].is_ascii_whitespace() {
            // A `token: value` line; the empty trailer config never promotes it
            // to a recognized prefix, matching git with no `trailer.*` set.
            trailer_lines += 1;
            possible_continuation_lines = 0;
        } else if line[0].is_ascii_whitespace() {
            possible_continuation_lines += 1;
        } else {
            non_trailer_lines += 1;
            non_trailer_lines += possible_continuation_lines;
            possible_continuation_lines = 0;
        }
        l = last_line(buf, bol);
    }
    len
}

/// Port of `find_separator()` (trailer.c) with the default `:` separator: the
/// index of the separator that ends the token, or `-1` if the line is not a
/// trailer. The token may contain alphanumerics, `-`, and internal whitespace.
fn find_separator(line: &[u8]) -> isize {
    let mut whitespace_found = false;
    for (idx, &c) in line.iter().enumerate() {
        if c == b':' {
            return idx as isize;
        }
        if !whitespace_found && (c.is_ascii_alphanumeric() || c == b'-') {
            continue;
        }
        if idx != 0 && (c == b' ' || c == b'\t') {
            whitespace_found = true;
            continue;
        }
        break;
    }
    -1
}

/// Port of `is_blank_line()` (trailer.c): a line (up to the next `\n` or the end
/// of the buffer) that is empty or all whitespace.
fn sig_is_blank_line(s: &[u8]) -> bool {
    for &c in s {
        if c == b'\n' {
            return true;
        }
        if !c.is_ascii_whitespace() {
            return false;
        }
    }
    true
}

/// Port of `next_line()` (trailer.c): the offset just past the next `\n` at or
/// after `off`, or `buf.len()` when there is none.
fn next_line_off(buf: &[u8], off: usize) -> usize {
    match buf[off..].iter().position(|&c| c == b'\n') {
        Some(p) => off + p + 1,
        None => buf.len(),
    }
}

/// Port of `last_line()` (trailer.c): the start offset of the last line within
/// `buf[..len]`, or `-1` when `len == 0`.
fn last_line(buf: &[u8], len: usize) -> isize {
    if len == 0 {
        return -1;
    }
    if len == 1 {
        return 0;
    }
    let mut i = len as isize - 2;
    while i >= 0 {
        if buf[i as usize] == b'\n' {
            return i + 1;
        }
        i -= 1;
    }
    0
}

/// Port of `ignore_non_trailer()` (builtin/commit.c): the number of trailing
/// bytes to ignore — a run of comment/blank lines (and an old `Conflicts:`
/// block) at the very end, or everything past a `>8` scissors line.
fn ignore_non_trailer(buf: &[u8]) -> usize {
    let len = buf.len();
    let mut boc = 0usize; // beginning of the trailing comment run (0 = none)
    let mut bol = 0usize;
    let mut in_conflicts = false;
    let cutoff = wt_status_locate_end(buf, "#");
    while bol < cutoff {
        let next = match buf[bol..].iter().position(|&c| c == b'\n') {
            Some(p) => bol + p + 1,
            None => len,
        };
        if buf[bol] == b'#' || buf[bol] == b'\n' {
            if boc == 0 {
                boc = bol;
            }
        } else if buf[bol..].starts_with(b"Conflicts:\n") {
            in_conflicts = true;
            if boc == 0 {
                boc = bol;
            }
        } else if in_conflicts && buf[bol] == b'\t' {
            // a pathname in the conflicts block — still part of the run
        } else if boc != 0 {
            boc = 0;
            in_conflicts = false;
        }
        bol = next;
    }
    if boc != 0 {
        len - boc
    } else {
        len - cutoff
    }
}

/// Port of `wt_status_locate_end()` (wt-status.c): the length up to a `>8`
/// scissors line (`# ------------------------ >8 ------------------------`), or
/// the full length when there is none. `comment` is git's `comment_line_str`.
fn wt_status_locate_end(s: &[u8], comment: &str) -> usize {
    let cut: &[u8] = b"------------------------ >8 ------------------------\n";
    let mut pattern: Vec<u8> = Vec::with_capacity(2 + comment.len() + cut.len());
    pattern.push(b'\n');
    pattern.extend_from_slice(comment.as_bytes());
    pattern.push(b' ');
    pattern.extend_from_slice(cut);
    if s.starts_with(&pattern[1..]) {
        return 0;
    }
    if let Some(p) = s
        .windows(pattern.len())
        .position(|w| w == pattern.as_slice())
    {
        return p + 1;
    }
    s.len()
}
