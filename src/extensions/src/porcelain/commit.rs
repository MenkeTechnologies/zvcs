use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::process::ExitCode;

/// `parse_options` rejecting an option: `error: <message>` and the usage table on
/// stderr, exit 129. Distinct from a `die()` — different stream shape, different
/// code — so it goes out here and unwinds carrying only the code.
fn usage_error(msg: String) -> anyhow::Error {
    eprintln!("error: {msg}");
    eprint!("{USAGE}");
    anyhow::Error::new(crate::fatal::Silent(129))
}

/// `usage_with_options()` rendering of `builtin/commit.c`'s option table,
/// verbatim. `parse_options` writes it after an `error:` line for an unknown
/// option or a malformed value, and to stdout for `-h`; both exit 129.
pub(super) const USAGE: &str = r"usage: git commit [-a | --interactive | --patch] [-s] [-v] [-u[<mode>]] [--amend]
                  [--dry-run] [(-c | -C | --squash) <commit> | --fixup [(amend|reword):]<commit>]
                  [-F <file> | -m <msg>] [--reset-author] [--allow-empty]
                  [--allow-empty-message] [--no-verify] [-e] [--author=<author>]
                  [--date=<date>] [--cleanup=<mode>] [--[no-]status]
                  [-i | -o] [--pathspec-from-file=<file> [--pathspec-file-nul]]
                  [(--trailer <token>[(=|:)<value>])...] [-S[<keyid>]]
                  [--] [<pathspec>...]

    -q, --[no-]quiet      suppress summary after successful commit
    -v, --[no-]verbose    show diff in commit message template

Commit message options
    -F, --[no-]file <file>
                          read message from file
    --[no-]author <author>
                          override author for commit
    --[no-]date <date>    override date for commit
    -m, --[no-]message <message>
                          commit message
    -c, --[no-]reedit-message <commit>
                          reuse and edit message from specified commit
    -C, --[no-]reuse-message <commit>
                          reuse message from specified commit
    --[no-]fixup [(amend|reword):]commit
                          use autosquash formatted message to fixup or amend/reword specified commit
    --[no-]squash <commit>
                          use autosquash formatted message to squash specified commit
    --[no-]reset-author   the commit is authored by me now (used with -C/-c/--amend)
    --[no-]trailer <trailer>
                          add custom trailer(s)
    -s, --[no-]signoff    add a Signed-off-by trailer
    -t, --[no-]template <file>
                          use specified template file
    -e, --[no-]edit       force edit of commit
    --[no-]cleanup <mode> how to strip spaces and #comments from message
    --[no-]status         include status in commit message template
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG sign commit

Commit contents options
    -a, --[no-]all        commit all changed files
    -i, --[no-]include    add specified files to index for commit
    --[no-]interactive    interactively add files
    -p, --[no-]patch      interactively add changes
    -U, --unified <n>     generate diffs with <n> lines context
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    -o, --[no-]only       commit only specified files
    -n, --no-verify       bypass pre-commit and commit-msg hooks
    --verify              opposite of --no-verify
    --[no-]dry-run        show what would be committed
    --[no-]short          show status concisely
    --[no-]branch         show branch information
    --[no-]ahead-behind   compute full ahead/behind values
    --[no-]porcelain      machine-readable output
    --[no-]long           show status in long format (default)
    -z, --[no-]null       terminate entries with NUL
    --[no-]amend          amend previous commit
    --no-post-rewrite     bypass post-rewrite hook
    --post-rewrite        opposite of --no-post-rewrite
    -u, --[no-]untracked-files[=<mode>]
                          show untracked files, optional modes: all, normal, no. (Default: all)
    --[no-]pathspec-from-file <file>
                          read pathspec from file
    --[no-]pathspec-file-nul
                          with --pathspec-from-file, pathspec elements are separated with NUL character

";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]allow-empty`, `--[no-]allow-empty-message`.
/// Captured byte-for-byte from stock git 2.55.0's `git commit --help-all`.
pub(super) const USAGE_ALL: &str = r#"usage: git commit [-a | --interactive | --patch] [-s] [-v] [-u[<mode>]] [--amend]
                  [--dry-run] [(-c | -C | --squash) <commit> | --fixup [(amend|reword):]<commit>]
                  [-F <file> | -m <msg>] [--reset-author] [--allow-empty]
                  [--allow-empty-message] [--no-verify] [-e] [--author=<author>]
                  [--date=<date>] [--cleanup=<mode>] [--[no-]status]
                  [-i | -o] [--pathspec-from-file=<file> [--pathspec-file-nul]]
                  [(--trailer <token>[(=|:)<value>])...] [-S[<keyid>]]
                  [--] [<pathspec>...]

    -q, --[no-]quiet      suppress summary after successful commit
    -v, --[no-]verbose    show diff in commit message template

Commit message options
    -F, --[no-]file <file>
                          read message from file
    --[no-]author <author>
                          override author for commit
    --[no-]date <date>    override date for commit
    -m, --[no-]message <message>
                          commit message
    -c, --[no-]reedit-message <commit>
                          reuse and edit message from specified commit
    -C, --[no-]reuse-message <commit>
                          reuse message from specified commit
    --[no-]fixup [(amend|reword):]commit
                          use autosquash formatted message to fixup or amend/reword specified commit
    --[no-]squash <commit>
                          use autosquash formatted message to squash specified commit
    --[no-]reset-author   the commit is authored by me now (used with -C/-c/--amend)
    --[no-]trailer <trailer>
                          add custom trailer(s)
    -s, --[no-]signoff    add a Signed-off-by trailer
    -t, --[no-]template <file>
                          use specified template file
    -e, --[no-]edit       force edit of commit
    --[no-]cleanup <mode> how to strip spaces and #comments from message
    --[no-]status         include status in commit message template
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG sign commit

Commit contents options
    -a, --[no-]all        commit all changed files
    -i, --[no-]include    add specified files to index for commit
    --[no-]interactive    interactively add files
    -p, --[no-]patch      interactively add changes
    -U, --unified <n>     generate diffs with <n> lines context
    --inter-hunk-context <n>
                          show context between diff hunks up to the specified number of lines
    -o, --[no-]only       commit only specified files
    -n, --no-verify       bypass pre-commit and commit-msg hooks
    --verify              opposite of --no-verify
    --[no-]dry-run        show what would be committed
    --[no-]short          show status concisely
    --[no-]branch         show branch information
    --[no-]ahead-behind   compute full ahead/behind values
    --[no-]porcelain      machine-readable output
    --[no-]long           show status in long format (default)
    -z, --[no-]null       terminate entries with NUL
    --[no-]amend          amend previous commit
    --no-post-rewrite     bypass post-rewrite hook
    --post-rewrite        opposite of --no-post-rewrite
    -u, --[no-]untracked-files[=<mode>]
                          show untracked files, optional modes: all, normal, no. (Default: all)
    --[no-]pathspec-from-file <file>
                          read pathspec from file
    --[no-]pathspec-file-nul
                          with --pathspec-from-file, pathspec elements are separated with NUL character
    --[no-]allow-empty    ok to record an empty change
    --[no-]allow-empty-message
                          ok to record a change with an empty message

"#;

use gix::bstr::{BString, ByteSlice};
use gix::index::entry::{Flags, Mode, Stage, Stat};
use gix::objs::tree::EntryMode;

use super::interpret_trailers::TrailerConfig;
use super::{Arg, LongOpt};

/// `cmd_commit()`'s `struct option builtin_commit_options[]` (builtin/commit.c),
/// in table order, as [`super::resolve_long`] reads it.
///
/// `--unified` and `--inter-hunk-context` come from `OPT_DIFF_UNIFIED` /
/// `OPT_DIFF_INTERHUNK_CONTEXT`, both `PARSE_OPT_NONEG`; `no-verify` and
/// `no-post-rewrite` are entries spelled with their own `no-`, which parse-options
/// reads as the *unset* sense of `verify` / `post-rewrite`.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "quiet",                       neg: true,  arg: Arg::None },
    LongOpt { name: "verbose",                     neg: true,  arg: Arg::None },
    LongOpt { name: "file",                        neg: true,  arg: Arg::Required },
    LongOpt { name: "author",                      neg: true,  arg: Arg::Required },
    LongOpt { name: "date",                        neg: true,  arg: Arg::Required },
    LongOpt { name: "message",                     neg: true,  arg: Arg::Required },
    LongOpt { name: "reedit-message",              neg: true,  arg: Arg::Required },
    LongOpt { name: "reuse-message",               neg: true,  arg: Arg::Required },
    LongOpt { name: "fixup",                       neg: true,  arg: Arg::Required },
    LongOpt { name: "squash",                      neg: true,  arg: Arg::Required },
    LongOpt { name: "reset-author",                neg: true,  arg: Arg::None },
    LongOpt { name: "trailer",                     neg: true,  arg: Arg::Required },
    LongOpt { name: "signoff",                     neg: true,  arg: Arg::None },
    LongOpt { name: "template",                    neg: true,  arg: Arg::Required },
    LongOpt { name: "edit",                        neg: true,  arg: Arg::None },
    LongOpt { name: "cleanup",                     neg: true,  arg: Arg::Required },
    LongOpt { name: "status",                      neg: true,  arg: Arg::None },
    LongOpt { name: "gpg-sign",                    neg: true,  arg: Arg::Optional },
    LongOpt { name: "all",                         neg: true,  arg: Arg::None },
    LongOpt { name: "include",                     neg: true,  arg: Arg::None },
    LongOpt { name: "interactive",                 neg: true,  arg: Arg::None },
    LongOpt { name: "patch",                       neg: true,  arg: Arg::None },
    LongOpt { name: "unified",                     neg: false, arg: Arg::Required },
    LongOpt { name: "inter-hunk-context",          neg: false, arg: Arg::Required },
    LongOpt { name: "only",                        neg: true,  arg: Arg::None },
    LongOpt { name: "no-verify",                   neg: true,  arg: Arg::None },
    LongOpt { name: "dry-run",                     neg: true,  arg: Arg::None },
    LongOpt { name: "short",                       neg: true,  arg: Arg::None },
    LongOpt { name: "branch",                      neg: true,  arg: Arg::None },
    LongOpt { name: "ahead-behind",                neg: true,  arg: Arg::None },
    LongOpt { name: "porcelain",                   neg: true,  arg: Arg::None },
    LongOpt { name: "long",                        neg: true,  arg: Arg::None },
    LongOpt { name: "null",                        neg: true,  arg: Arg::None },
    LongOpt { name: "amend",                       neg: true,  arg: Arg::None },
    LongOpt { name: "no-post-rewrite",             neg: true,  arg: Arg::None },
    LongOpt { name: "untracked-files",             neg: true,  arg: Arg::Optional },
    LongOpt { name: "pathspec-from-file",          neg: true,  arg: Arg::Required },
    LongOpt { name: "pathspec-file-nul",           neg: true,  arg: Arg::None },
    LongOpt { name: "allow-empty",                 neg: true,  arg: Arg::None },
    LongOpt { name: "allow-empty-message",         neg: true,  arg: Arg::None },
];
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

/// `sign_buffer()`'s backend for this repository, with `-S<keyid>` folded in.
///
/// The whole `gpg.format` table lives in [`crate::gitsig::Signer`] — `openpgp`
/// signs with `gpg.program` / `gpg.openpgp.program` (default `gpg`), `x509` with
/// `gpg.x509.program` (default `gpgsm`, which `gpg.program` does *not* override),
/// and `ssh` with `gpg.ssh.program` (default `ssh-keygen`) through an entirely
/// different argument vector that leaves an `SSH SIGNATURE` block rather than an
/// armored PGP one. Resolving only `gpg.program` here, as this used to, meant
/// `git commit -S` under `gpg.format = ssh` ran `gpg -bsa` against an ssh *public
/// key* and died with `gpg: skipped …: No secret key`.
///
/// `sign_buffer(…, signing_key, SIGN_BUFFER_USE_DEFAULT_KEY)` (gpg-interface.c:977)
/// uses the caller's key when it is non-empty and falls back to `get_signing_key()`
/// otherwise, which is exactly what overwriting `key` here expresses: `-S<keyid>`
/// wins over `user.signingKey`, and neither leaves the format's own default in
/// charge.
fn resolve_signer(repo: &gix::Repository, key: Option<String>) -> crate::gitsig::Signer {
    let mut signer = crate::gitsig::Signer::resolve(repo);
    if let Some(key) = key.filter(|k| !k.is_empty()) {
        signer.key = Some(key);
    }
    signer
}

/// `opts->gpg_sign` as a signing backend, for the sequencer's in-process commit.
///
/// `try_to_commit()` hands the field straight to `commit_tree_extended()`
/// (sequencer.c:1685), whose `sign_commit` parameter is NULL-or-key with exactly
/// the same three states: absent means do not sign, empty means sign with
/// `get_signing_key()`'s choice, and a key names one.
pub(crate) fn sequencer_signer(
    repo: &gix::Repository,
    gpg_sign: Option<&str>,
) -> Option<crate::gitsig::Signer> {
    gpg_sign.map(|key| resolve_signer(repo, Some(key.to_string())))
}

/// The signer a plain `git commit` with no `-S` would use: `commit.gpgSign` and
/// nothing else.
///
/// `continue_single_pick()` (sequencer.c:5232-5257) spawns
/// `git commit [--no-edit --cleanup=strip]` and deliberately pushes **no** `-S`,
/// unlike `run_git_commit()` two functions up. So a stopped pick that resumes is
/// signed exactly when the *config* asks for it — the `-S` that started the
/// sequence does not reach it.
pub(crate) fn commit_config_signer(repo: &gix::Repository) -> Option<crate::gitsig::Signer> {
    (repo.config_snapshot().boolean("commit.gpgSign") == Some(true))
        .then(|| resolve_signer(repo, None))
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
    /// `--amend`, which points the report at `HEAD^1` instead of `HEAD`
    /// (builtin/commit.c:571-574).
    amend: bool,
    /// `-v`/`--verbose`, which `run_status()` forwards as `s->verbose`
    /// (builtin/commit.c:575) so the report ends with the staged patch.
    verbose: bool,
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
/// `lookup_commit_or_die(&oid, "HEAD")` (commit.c:81-91) as `cmd_commit()` calls
/// it (builtin/commit.c:1816): `HEAD` may be unborn, but if it names an object
/// that object has to be a commit.
///
/// Returns `Some(128)` once the diagnostics are on stderr, `None` when there is
/// nothing to complain about. The two shapes are `lookup_commit_reference_gently`'s
/// (commit.c:50-67):
///
///   * a `HEAD` the odb cannot find fails inside `peel_object_ext()`, which takes
///     the `default: return NULL` arm *before* the type test — so only
///     `die(_("could not parse %s"), ref_name)` is printed;
///   * a `HEAD` on a blob or a tree peels fine and then fails the `type !=
///     OBJ_COMMIT` test, which prints `error(_("object %s is a %s, not a %s"))`
///     first and lets the `die()` follow.
///
/// Neither is a `status` diagnostic: `git status` in the same repository peels
/// `HEAD` to a *tree* and reports normally (see [`super::status`]). It is
/// `git commit` alone that needs a commit, because it is about to take one as a
/// parent.
fn die_unless_head_is_a_commit(repo: &gix::Repository) -> Result<Option<ExitCode>> {
    // `repo_get_oid()` resolving is the whole of the unborn test; it reads no
    // object, so a dangling `HEAD` gets past it exactly as git's does.
    let Ok(id) = repo.rev_parse_single("HEAD") else {
        return Ok(None);
    };
    let kind = match repo.try_find_object(id.detach())? {
        Some(object) => object.peel_tags_to_end().ok().map(|peeled| peeled.kind),
        None => None,
    };
    match kind {
        Some(gix::object::Kind::Commit) => Ok(None),
        Some(other) => {
            eprintln!("error: object {} is a {other}, not a commit", id.detach());
            eprintln!("fatal: could not parse HEAD");
            Ok(Some(ExitCode::from(128)))
        }
        None => {
            eprintln!("fatal: could not parse HEAD");
            Ok(Some(ExitCode::from(128)))
        }
    }
}

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
                    message: Default::default(),
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
pub(super) fn die_resolve_conflict(index: &gix::index::File) -> ExitCode {
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
/// `user.signingKey`) writes a `gpgsig` header through [`crate::gitsig::Signer`],
/// so the whole `gpg.format` table applies: `openpgp` via `gpg.program` or
/// `gpg.openpgp.program`, `x509` via `gpg.x509.program` (default `gpgsm`), and
/// `ssh` via `gpg.ssh.program` (default `ssh-keygen`), which produces an
/// `SSH SIGNATURE` block rather than an armored PGP one.
///
/// `post-commit` runs before the `post-rewrite amend` an `--amend` fires, which
/// is git's order (builtin/commit.c:1966-1970) and matters because each hook can
/// see what the other left.
///
/// `core.commentChar = auto` is honoured: [`adjust_comment_line_char`] re-picks
/// the comment character against the message body once it exists, so a body line
/// starting with `#` is text rather than something the `strip` cleanup deletes.
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
        // A value still owed to `-U`/`--unified`/`--inter-hunk-context` is taken
        // verbatim, even past `--`, the way parse-options takes it — and precisely
        // because it is a value, it is never resolved as an option name.
        if patch_opts.awaiting_value() {
            match patch_opts.take_arg(args[i].as_str()) {
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
        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): tested on the token as typed, ahead of the
        // abbreviation resolver, because it is a `strcmp` — `--help-a` and
        // `--help-all=x` stay unknown options. It renders `USAGE_FULL`, which
        // for `commit` keeps the hidden `--allow-empty[-message]`.
        if args[i] == "--help-all" {
            return Ok(super::show_usage(USAGE_ALL));
        }
        // `at` is this argument's own index; `i` steps past it here so that it is
        // already `parse_opt_ctx_t`'s "next unread argument" and the shared port
        // of `get_arg()` can advance it over a value. Every value-taking option
        // below used to read `args.get(i)` after its own `i += 1` and word its
        // own refusal, which is how they ended up saying ``option `-m'`` where
        // stock says ``switch `m'``.
        let at = i;
        i += 1;
        // Respell a unique abbreviation as the name it resolves to, ahead of both
        // the shared value-option handler and the match below, so `--allow-empty-m`
        // reaches the same arm as `--allow-empty-message`.
        let canonical;
        let a = match super::canonical_long(args[at].as_str(), LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(&args[at], &first, &second, USAGE))
            }
        };
        match patch_opts.take_arg(a) {
            Err(code) => return Ok(code),
            Ok(true) => continue,
            Ok(false) => {}
        }
        match a {
            "-m" | "--message" => messages.push(super::take_value(args, &mut i, a)?.to_string()),
            "-F" | "--file" => file_args.push(super::take_value(args, &mut i, a)?.to_string()),
            "-C" | "--reuse-message" => {
                reuse_arg = Some(super::take_value(args, &mut i, a)?.to_string())
            }
            "-c" | "--reedit-message" => {
                reuse_arg = Some(super::take_value(args, &mut i, a)?.to_string());
                reedit = true;
            }
            "--date" => date_arg = Some(super::take_value(args, &mut i, a)?.to_string()),
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
            // The unset halves of the message-source options. Each is exactly what
            // parse-options writes for that entry's type, so a later `--no-<x>`
            // discards an earlier value rather than being an unknown option:
            //
            //   `-m` is an `OPT_CALLBACK` over `opt_parse_m()`, whose unset arm
            //     clears the accumulated buffer (builtin/commit.c:172-174);
            //   `-F`/`--file` is an `OPT_FILENAME` and `--author`, `--date`,
            //     `-c`, `-C`, `--fixup`, `--squash` are `OPT_STRING`s, whose unset
            //     writes NULL over the slot (parse-options.c:200-202, 214-215);
            //   `--reset-author` and the two hidden `--allow-empty*` are
            //     `OPT_BOOL`/`OPT_HIDDEN_BOOL`, whose unset writes 0;
            //   `-q` is an `OPT__QUIET` (`OPT_COUNTUP`), whose unset resets to 0.
            "--no-message" => messages.clear(),
            "--no-file" => file_args.clear(),
            "--no-reuse-message" | "--no-reedit-message" => {
                reuse_arg = None;
                reedit = false;
            }
            "--no-date" => date_arg = None,
            "--no-author" => author_arg = None,
            "--no-fixup" => fixup_arg = None,
            "--no-squash" => squash_arg = None,
            "--no-reset-author" => reset_author = false,
            "--no-allow-empty" => allow_empty = false,
            "--no-allow-empty-message" => allow_empty_message = false,
            "--no-quiet" => quiet = false,
            "-a" | "--all" => all = true,
            "--no-all" => all = false,
            // `-n`/`--no-verify` skips `pre-commit` + `commit-msg`; `--verify` is
            // its opposite, and the last one on the command line wins.
            "-n" | "--no-verify" => verify = false,
            "--verify" => verify = true,
            "-s" | "--signoff" => signoff = true,
            "--no-signoff" => signoff = false,
            "--squash" => squash_arg = Some(super::take_value(args, &mut i, a)?.to_string()),
            "--fixup" => fixup_arg = Some(super::take_value(args, &mut i, a)?.to_string()),
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
            "--author" => author_arg = Some(super::take_value(args, &mut i, a)?.to_string()),
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
                {
                    // A malformed *value* is an `error:` and 129, but without the
                    // usage table — `parse_options` only appends that when it
                    // cannot make sense of the option itself.
                    eprintln!("error: option `porcelain' takes no value");
                    return Err(anyhow::Error::new(crate::fatal::Silent(129)));
                }
            }
            "-z" | "--null" => null_term = true,
            "--no-null" => null_term = false,
            // `--branch` has no short name in `builtin_commit_options`; `-b` used
            // to be accepted here and silently ran the commit, where stock refuses
            // it with ``unknown switch `b'``.
            "--branch" => branch_header = Some(true),
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
                pathspec_from_file = Some(super::take_value(args, &mut i, a)?.to_string())
            }
            s if s.starts_with("--pathspec-from-file=") => {
                pathspec_from_file = Some(s["--pathspec-from-file=".len()..].to_string())
            }
            "--no-pathspec-from-file" => pathspec_from_file = None,
            "--pathspec-file-nul" => pathspec_file_nul = true,
            "--no-pathspec-file-nul" => pathspec_file_nul = false,
            // --- message shaping -----------------------------------------------
            "--cleanup" => cleanup_arg = Some(super::take_value(args, &mut i, a)?.to_string()),
            s if s.starts_with("--cleanup=") => {
                cleanup_arg = Some(s["--cleanup=".len()..].to_string())
            }
            "--no-cleanup" => cleanup_arg = None,
            "--trailer" => trailer_args.push(super::take_value(args, &mut i, a)?.to_string()),
            s if s.starts_with("--trailer=") => {
                trailer_args.push(s["--trailer=".len()..].to_string())
            }
            "--no-trailer" => trailer_args.clear(),
            "-t" | "--template" => template_arg = Some(super::take_value(args, &mut i, a)?.to_string()),
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
            s if s.starts_with("--") => {
                return Err(usage_error(format!("unknown option `{}'", s.trim_start_matches('-'))))
            }
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
                for (off, c) in cluster.char_indices() {
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
                        // Optional-value short flags: bare in a cluster they take
                        // their default, an attached value ends the cluster.
                        'u' | 'S' => {
                            let rest = &cluster[off + c.len_utf8()..];
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
                            let rest = &cluster[off + c.len_utf8()..];
                            let val = match rest.is_empty() {
                                // `optname(opt, OPT_SHORT)`: the character, not
                                // the token — ``switch `m'``, never ``option `-m'``.
                                true => crate::parseopt::get_arg(
                                    args,
                                    &mut i,
                                    crate::parseopt::OptName::Short(c),
                                )?
                                .to_string(),
                                false => rest.to_string(),
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
                        // parse_options_step() tests `internal_help` inside the
                        // short-option loop, so `-h` answers wherever it lands
                        // in a cluster — on stdout at 129, with no `error:` line.
                        'h' => return Ok(super::show_usage(USAGE)),
                        // `PARSE_OPT_UNKNOWN` for the character parsing stopped
                        // at, against the synthetic `-<rest>` token the C builds
                        // at parse-options.c:1095 — which also carries the
                        // non-ASCII case, where git names the whole token.
                        _ => {
                            return Ok(super::unknown_option(
                                &format!("-{}", &cluster[off..]),
                                USAGE,
                            ))
                        }
                    }
                }
            }
            // A bare positional argument is a pathspec (git's `--only` mode).
            _ => pathspecs.push(args[at].clone()),
        }
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
        crate::git_fatal!("options '-i/--include' and '-o/--only' cannot be used together");
    }
    if all && only_flag {
        crate::git_fatal!("options '-o/--only' and '-a/--all' cannot be used together");
    }
    if all && include_flag {
        crate::git_fatal!("options '-i/--include' and '-a/--all' cannot be used together");
    }
    if include_flag && interactive {
        crate::git_fatal!(
            "options '-i/--include' and '--interactive/-p/--patch' cannot be used together"
        );
    }
    if only_flag && interactive {
        crate::git_fatal!("options '-o/--only' and '--interactive/-p/--patch' cannot be used together");
    }
    if all && interactive {
        crate::git_fatal!("options '-a/--all' and '--interactive/-p/--patch' cannot be used together");
    }

    // git's `prepare_index()` opens with the two `cannot be negative` fatals.
    if let Some(code) = patch_opts.reject_negative() {
        return Ok(code);
    }
    // `--pathspec-from-file` supplies the pathspec list instead of the command
    // line, so it is resolved before every pathspec-dependent check below.
    if pathspec_from_file.is_some() && !pathspecs.is_empty() {
        crate::git_fatal!("'--pathspec-from-file' and pathspec arguments cannot be used together");
    }
    if pathspec_file_nul && pathspec_from_file.is_none() {
        crate::git_fatal!("the option '--pathspec-file-nul' requires '--pathspec-from-file'");
    }
    if let Some(src) = &pathspec_from_file {
        if interactive {
            crate::git_fatal!(
                "options '--pathspec-from-file' and '--interactive/--patch' cannot be used together"
            );
        }
        if all {
            crate::git_fatal!("options '--pathspec-from-file' and '-a' cannot be used together");
        }
        pathspecs = read_pathspec_file(src, pathspec_file_nul)?;
    }
    // `builtin/commit.c:parse_and_validate_options`:
    //     if (argc == 0 && (also || (only && !amend && !allow_empty)))
    //             die(_("No paths with --include/--only does not make sense."));
    // `--only` with no paths is how a caller says "commit exactly what is staged
    // and nothing the worktree has since changed", which is meaningful the moment
    // there is something to commit without a pathspec — an amend, or an explicitly
    // allowed empty commit. `--include` has no such reading and always needs paths.
    //
    // Rejecting the amend form broke `commit --amend -F <file> --only`, which is
    // how the JetBrains client rewords a commit message.
    if pathspecs.is_empty() && (include_flag || (only_flag && !amend && !allow_empty)) {
        crate::git_fatal!("No paths with --include/--only does not make sense.");
    }
    // `parse_and_validate_options()` rejects a malformed `-u<mode>` and
    // `--cleanup=<mode>` before the index is read, so the answer does not depend
    // on whether there was anything to commit. Validating them where they are
    // *used* put both behind the "nothing to commit" exit, which meant a typo in
    // either was reported as a clean tree.
    if let Some(u) = &untracked_arg {
        if !matches!(u.as_str(), "no" | "normal" | "all") {
            crate::git_fatal!("Invalid untracked files mode '{u}'");
        }
    }
    if let Some(c) = &cleanup_arg {
        if !matches!(
            c.as_str(),
            "default" | "verbatim" | "whitespace" | "strip" | "scissors"
        ) {
            crate::git_fatal!("Invalid cleanup mode {c}");
        }
    }
    // Outside patch mode the two diff-shaping options have nothing to feed, and
    // git refuses the whole command rather than ignore them.
    if let Some(code) = patch_opts.require_patch_only(interactive, "--interactive/--patch") {
        return Ok(code);
    }
    // `git commit -a <paths>` is rejected outright, exactly as git does.
    if all && !pathspecs.is_empty() {
        crate::git_fatal!("paths '{} ...' with -a does not make sense", pathspecs[0]);
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
    // The object this writes carries an identity, and git fills the halves
    // the user did not give rather than refusing — except under
    // `user.useConfigOnly`, which is the one case it says so.
    let mut repo = gix::discover(".")?;
    if let Some(code) = crate::ensure_object_identity(&mut repo, "Author") {
        return Ok(code);
    }
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

    // ```c
    // if (repo_get_oid(the_repository, "HEAD", &oid))
    //         current_head = NULL;
    // else {
    //         current_head = lookup_commit_or_die(&oid, "HEAD");
    // ```
    //
    // (builtin/commit.c:1813-1816.) `cmd_commit()`'s very first act, before it
    // has even parsed its options. `repo_get_oid()` only turns the name into an
    // oid, so an unborn `HEAD` is the ordinary root-commit case; anything else
    // has to be a commit, and `lookup_commit_reference()` says so in two
    // separate voices when it is not (commit.c:61-67, :84-85):
    //
    //   * the object exists but is a blob/tree — `error("object %s is a %s, not
    //     a %s")` first, then `die("could not parse HEAD")`;
    //   * the object is missing entirely — `peel_object_ext()` fails before the
    //     type test, so only the `die()` is printed.
    //
    // Both exit 128, and both happen here rather than at the first use, which is
    // why a `--dry-run` of a repository in this state never reaches the report.
    if let Some(code) = die_unless_head_is_a_commit(&repo)? {
        return Ok(code);
    }

    // `status_init_config(&s, git_commit_config)` (builtin/commit.c:1808) chains
    // into `git_status_config`, so `git commit` validates `status.showUntrackedFiles`
    // exactly as `git status` does — and dies on a bad value even when no report
    // will be rendered, as a plain `-m <msg>` commit does not.
    if let Some(code) = super::status::validate_show_untracked_files(&repo) {
        return Ok(code);
    }

    // --- `determine_whence()` --------------------------------------------
    // A merge, cherry-pick, revert or rebase left in the index is what this
    // commit concludes; everything downstream (parents, default message, which
    // options are legal, what state is torn down) keys off this.
    let whence = determine_whence(&repo);

    // `parse_and_validate_options()`: an in-progress operation forbids `--amend`,
    // because the commit being replaced is not the one the operation is building.
    if amend && whence != Whence::Commit {
        crate::git_fatal!("You are in the middle of a {} -- cannot amend.", whence.noun());
    }
    // `prepare_index()`: a pathspec-limited commit builds a tree that ignores the
    // rest of the index, which would silently drop the operation's other paths.
    if only_mode && whence != Whence::Commit {
        crate::git_fatal!("cannot do a partial commit during a {}.", whence.noun());
    }

    // --- `-p`/`--interactive`: hand staging to the hunk selector ----------
    // git's `prepare_index()` runs `interactive_add()` before anything reads the
    // index, so `--dry-run` reaches it too and then throws the selection away
    // with the rest of the prepared index (`rollback_index_files()`); the guard
    // below lives to the end of this function and does exactly that unless the
    // commit succeeds.
    let mut interactive_stage = None;
    if interactive {
        let mut guard = InteractiveStage::hold(&repo, patch_interactive)?;
        let status = if patch_interactive {
            // git's `interactive_add()` under a `GIT_INDEX_FILE` pointed at the
            // prepared copy: the selector's `apply --cached` children stage there
            // and the repository's index is untouched until [`InteractiveStage::adopt`].
            super::add_patch::run_in_index(
                &repo,
                super::add_patch::Mode::Add,
                guard.staging_index(),
                patch_opts.to_interactive(false),
                &pathspecs,
            )?
        } else {
            super::add_interactive::run_status(&repo, patch_opts.to_interactive(false), &pathspecs)?
        };
        if status != 0 {
            crate::git_fatal!("interactive add failed");
        }
        // `read_index_from(get_lock_file_path(&index_lock))`: the selection becomes
        // the index the tree is built from below.
        guard.adopt()?;
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
            crate::git_fatal!("You have nothing to amend.");
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
                amend,
                // `if (verbose == -1) verbose = config_commit_verbose` runs in
                // `cmd_commit` *before* it branches to `dry_run_commit()`
                // (builtin/commit.c:1827-1828), so `commit.verbose` reaches the dry
                // run just as `-v` does.
                verbose: verbose.unwrap_or_else(|| {
                    repo.config_snapshot().boolean("commit.verbose") == Some(true)
                }),
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
            crate::git_fatal!("empty commit message (use --allow-empty-message to override)");
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
        crate::git_fatal!("options '--squash' and '--fixup' cannot be used together");
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
            crate::git_fatal!("options '-c/-C/-F' and '--fixup' cannot be used together");
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
                    _ => crate::git_fatal!("unknown option: --fixup={sub}:{commit}"),
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
                crate::git_fatal!("options '-m' and '--fixup=amend:<commit>' cannot be used together");
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
    // `--date=<date>` overrides the author date. `parse_force_date()` (builtin/commit.c:614)
    // tries `parse_date()` first and only falls back to `approxidate_careful()`, failing when
    // that reports an error — so `--date=0` is the current time, not the epoch.
    let date_override: Option<gix::date::Time> = match &date_arg {
        Some(d) => Some(match crate::date::parse_date_basic(d) {
            Some(time) => time,
            None => match crate::date::approxidate_careful(d) {
                (seconds, false) => gix::date::Time::new(seconds, 0),
                (_, true) => crate::git_fatal!("invalid date format: {d}"),
            },
        }),
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
        // `-i` writes the real index through `write_locked_index()`
        // (builtin/commit.c:454-465), so the repository's index-write settings apply:
        // `do_write_index()` takes `skip_hash` from the settings block for every
        // index it serialises (read-cache.c:2830-2831).
        index.write(crate::config::index_write_options(&repo))?;
    }

    // --- build a tree object from the index ------------------------------
    // A freshly-init'd repo has no index file yet, which is an empty index — git's
    // root empty commit (`commit --allow-empty` on a fresh repo) then produces the
    // empty tree instead of failing to open a file that isn't there.
    let mut index = open_or_empty_index(&repo)?;

    // Refuse while conflicts are staged, exactly as git does — `refresh_cache_or_die()`
    // reports every unmerged path and then `die_resolve_conflict("commit")`.
    if index
        .entries()
        .iter()
        .any(|e| e.stage() != gix::index::entry::Stage::Unconflicted)
    {
        return Ok(die_resolve_conflict(&index));
    }

    // `pre-commit` runs before the commit is built; a non-zero exit aborts it
    // (the hook prints its own diagnostics, so we exit quietly). `--no-verify`
    // skips it, as it does `commit-msg`.
    if verify && !crate::hooks::run(&repo, "pre-commit", &[], None)? {
        return Ok(ExitCode::from(1));
    }

    // The tree a commit records *is* the index's cache-tree root:
    // `commit_tree_extended(..., &the_repository->index->cache_tree->oid, ...)`
    // (builtin/commit.c:1938), fed by the `cache_tree_update()` that
    // `prepare_to_commit()` runs over the index it just read (builtin/commit.c:1111).
    //
    // The as-is path of `prepare_index()` (builtin/commit.c:482-491) is what decides
    // whether the index file is rewritten: it updates the cache-tree when the index
    // changed *or* the cache-tree is not fully valid, and `cache_tree_update()`
    // setting `CACHE_TREE_CHANGED` is then what gets past the `SKIP_IF_UNCHANGED`
    // guard in `write_locked_index()` (read-cache.c:3333). Everything before this
    // point has already written whatever it staged (`-a` through
    // `stage_tracked_changes()`, `-i` through `include_stage()`) and re-read it, so
    // the in-memory index equals the on-disk one and git's `cache_changed` is zero:
    // the condition reduces to "the cache-tree is not fully valid", which
    // [`super::write_tree::refresh_cache_tree`] tests and acts on. A commit that
    // follows a `git add` therefore rewrites the index — the `add` invalidated the
    // root — while a second `commit --allow-empty` in a row leaves it alone.
    //
    // In pathspec-limited ("only") mode the tree comes from HEAD's tree with only
    // the listed paths swapped for their worktree content instead — see
    // `build_only_mode_tree`, which also stages those paths into the real index.
    let tree_id: ObjectId = if only_mode {
        build_only_mode_tree(&repo, &pathspecs)?
    } else {
        match super::write_tree::refresh_cache_tree(&repo, &mut index, false)? {
            Ok(id) => id,
            Err(err) => {
                // `if (cache_tree_update(the_repository->index, 0)) { error(_("Error
                // building trees")); return 0; }` (builtin/commit.c:1111-1114), and a
                // `prepare_to_commit()` that returns 0 makes `cmd_commit` exit 1.
                super::write_tree::report_tree_build_failure(&err);
                eprintln!("error: Error building trees");
                return Ok(ExitCode::from(1));
            }
        }
    };

    // --- parents ---------------------------------------------------------
    // `--amend` replaces HEAD: the new commit takes HEAD's *parents*, and the
    // summary/nothing-to-commit checks compare against HEAD's first parent tree,
    // not HEAD itself.
    let mut head = repo.head()?;
    let head_tip = head.try_peel_to_id()?.map(|id| id.detach());
    let amend_head = if amend {
        // `die(_("You have nothing to amend."))` (builtin/commit.c), so this is a
        // `fatal:` at 128 rather than this port's own error voice at 1.
        let Some(hid) = head_tip else {
            crate::git_fatal!("You have nothing to amend.");
        };
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
    // git's `initial_commit`, which is `!current_head` — whether `HEAD` existed
    // *before* this command, not whether the commit being written has parents.
    // The two differ for `--amend` of a root commit: the result has no parent, but
    // `HEAD` was there, so git prints neither `(root-commit)` in the summary nor
    // "Initial commit" in the status block.
    let is_root = head_tip.is_none();
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
    // git's guard is `!committable && whence != FROM_MERGE && !allow_empty &&
    // !(amend && is_a_merge(current_head))` (builtin/commit.c:1081-1082).
    // Concluding a merge is exempt because resolving every conflict back to
    // `HEAD`'s content still has to record the merge, and *amending* a merge is
    // exempt for the same reason — the commit being rewritten is a merge, so it
    // stays one whatever its tree says.
    let amending_a_merge = amend && parents.len() > 1;
    if unchanged && !allow_empty && whence != Whence::Merge && !amending_a_merge {
        // `run_status(stdout, index_file, prefix, 0, s)` (builtin/commit.c:1085).
        // The refusal *is* a status report: git runs the engine `git status` runs,
        // over the index this commit would have used, on stdout — and only then
        // adds whatever advice the situation calls for on stderr. Nothing else is
        // printed for a plain empty commit, so this report is the whole message.
        //
        // An `--amend` refusal takes the same route: `s->amend`/`s->reference` were
        // set by `run_status()` long before the guard ran, so the report is the
        // `HEAD^1` one and only the advice underneath it differs. What git does
        // *not* do here is `die()` — the advice goes to stderr with no `fatal:` in
        // front of it and the exit code is 1, not 128.
        //
        // The long format is the only one reachable: a `--short`/`--porcelain`/
        // `-z` request has already turned the command into a dry run
        // (builtin/commit.c:1422-1423) and returned far above.
        report_nothing_to_commit(
            untracked_arg.as_deref(),
            match amend {
                true => super::status::Reference::AmendParent,
                false => super::status::Reference::Commit,
            },
        )?;
        if amend {
            // `fputs(_(empty_amend_advice), stderr)` (builtin/commit.c:1086-1087),
            // whose text is a single trailing-newline-terminated block.
            eprint!(
                "You asked to amend the most recent commit, but doing so would make\n\
                 it empty. You can repeat your command with --allow-empty, or you can\n\
                 remove the commit entirely with \"git reset HEAD^\".\n"
            );
            return Ok(ExitCode::from(1));
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
        // `prepare_to_commit()` returns 0, which `cmd_commit()` turns into
        // `ret = 1` — an exit code and not a word of its own on stderr.
        return Ok(ExitCode::from(1));
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
    // `auto` is resolved to `#` at config time and only becomes something else
    // once `prepare_to_commit()` can see the message body, below.
    let (mut comment, comment_char_is_auto) = comment_prefix_full(&snap);
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
        let ignore_footer = ignore_non_trailer(buf.as_bytes());
        append_signoff(&mut buf, &ident, ignore_footer, false);
    }

    // ```c
    // if (fwrite(sb.buf, 1, sb.len, s->fp) < sb.len)
    //         die_errno(_("could not write commit template"));
    // if (auto_comment_line_char)
    //         adjust_comment_line_char(&sb);
    // ```
    //
    // (builtin/commit.c:931-937.) Exactly here: after `-s` has appended its
    // trailer and before anything commented is added, so the character is chosen
    // against the user's text alone. It runs whether or not an editor will, since
    // the cleanup that follows uses the same character.
    if comment_char_is_auto {
        comment = adjust_comment_line_char(&buf)?;
    }

    // `--author="Name <email>"` overrides the author identity. The author *date*
    // is unchanged: HEAD's on an amend (git preserves it), the configured author
    // time (now / GIT_AUTHOR_DATE) on a new commit.
    //
    // Resolved here rather than at the write, because `prepare_to_commit()`'s
    // editor block names the author (builtin/commit.c:998-1004) — git has had it
    // since `determine_author_info()` ran inside `parse_and_validate_options()`,
    // long before the template.
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
    let cfg_author = || -> Result<gix::actor::Signature> {
        Ok(repo
            .author()
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("unable to determine author identity"))?
            .to_owned()?)
    };
    let author_owned: Option<gix::actor::Signature> = if needs_author {
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

    // The commented help + status block, and the `-v` diff below the cut line, go
    // into the editor buffer only — git gates both on `use_editor && include_status`.
    // `git_path_commit_editmsg()`, which is absolute: `setup_git_directory()`
    // makes `$GIT_DIR` absolute during startup, so every consumer of the path —
    // the editor, `prepare-commit-msg`, `commit-msg` — is handed one that does
    // not depend on their working directory. gix reports a discovered git
    // directory relative to the cwd, so rebase it here rather than at each use.
    let msg_path: std::path::PathBuf = {
        let dir = repo.git_dir();
        let absolute = match dir.is_absolute() {
            true => dir.to_owned(),
            false => std::env::current_dir()?.join(dir),
        };
        // gix reports a discovered git directory as `./.git`; `normalize_path()`
        // drops the `.` on git's side, and `Components` does the same here.
        absolute.components().collect::<std::path::PathBuf>().join("COMMIT_EDITMSG")
    };
    if use_editor && include_status {
        if !buf.is_empty() && !buf.ends_with('\n') {
            buf.push('\n');
        }
        // `author_date_is_interesting()` (builtin/commit.c:694) is
        // `author_message || force_date`, and `author_message` is set exactly
        // when the authorship is inherited rather than composed: `-C`/`-c`
        // (builtin/commit.c:1358-1363), the `use_message = "HEAD"` an `--amend`
        // without one implies (:1353-1354), or a cherry-pick / rebase pick
        // (:1365-1368) — each of them disarmed by `--reset-author`.
        let author_message = !reset_author
            && (reuse_commit.is_some()
                || (amend && fixup_arg.is_none())
                || whence.is_cherry_pick()
                || whence.is_rebase());
        let author = match &author_owned {
            Some(a) => a.clone(),
            None => cfg_author()?,
        };
        buf.push_str(&editor_status_block(
            &repo,
            &comment,
            cleanup,
            whence,
            &author,
            author_message || date_override.is_some(),
            untracked_arg.as_deref(),
            match amend {
                true => super::status::Reference::AmendParent,
                false => super::status::Reference::Commit,
            },
        )?);
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

    // ```c
    // if (launch_editor(git_path_commit_editmsg(), NULL, env.v)) {
    //         fprintf(stderr,
    //         _("Please supply the message using either -m or -F option.\n"));
    //         exit(1);
    // }
    // ```
    //
    // (builtin/commit.c:1124-1127.) The editor's own `error:` line is already on
    // stderr; this is the hint that follows it, and the status is 1 — not 128.
    if use_editor {
        if launch_editor(&snap, &msg_path).is_err() {
            eprintln!("Please supply the message using either -m or -F option.");
            return Ok(ExitCode::from(1));
        }
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
            crate::git_fatal!("empty commit message (use --allow-empty-message to override)");
        }
        // Not a `die()`: `commit.c:1906-1909` writes this with `fprintf(stderr,
        // …)` and calls `exit(1)`, so it carries no `fatal:` prefix and exits 1,
        // exactly like the untouched-template abort a few lines above. Reachable
        // whenever the message clears — `--no-message` after a `-m`, an emptied
        // editor buffer, an all-comment `-F` file.
        eprintln!("Aborting commit due to empty commit message.");
        return Ok(ExitCode::from(1));
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
    // `print_commit_summary()` renders `%s`, which is `format_subject(sb, msg,
    // " ")` — the *whole* first paragraph folded onto one line, not just its
    // first line. A subject written across two lines prints as one.
    let subject = folded_subject(&message);

    let committer_owned = || -> Result<gix::actor::Signature> {
        Ok(repo
            .committer()
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("unable to determine committer identity"))?
            .to_owned()?)
    };

    // `-S`/`--gpg-sign[=<keyid>]` (or `commit.gpgSign`) makes the object carry a
    // `gpgsig` header; the key is the flag's, else `user.signingKey`, and the
    // program and signature format come from the `gpg.format` backend.
    // `None` leaves the untouched `Repository::commit` fast paths in charge, so
    // an unsigned commit is byte-for-byte as before.
    let signer: Option<crate::gitsig::Signer> = match gpg_sign {
        GpgSign::Off => None,
        GpgSign::Unset if snap.boolean("commit.gpgSign") != Some(true) => None,
        GpgSign::Unset => Some(resolve_signer(&repo, None)),
        GpgSign::On(key) => Some(resolve_signer(&repo, key)),
    };

    // git's `reflog_msg` (builtin/commit.c:1850-1894): `GIT_REFLOG_ACTION` when
    // the environment names one — it is read *before* every fallback, so it wins
    // over all of them — else "commit", "commit (initial)", "commit (amend)",
    // "commit (merge)", "commit (cherry-pick)" or "commit (rebase)". gix derives
    // the first four from the parent count on its own, so only the sequencer's two
    // need to be supplied — and supplying one forces the explicit write path below.
    //
    // The sequencer sets `GIT_REFLOG_ACTION=revert`/`cherry-pick` on the `git
    // commit` it spawns (sequencer.c:1141), which is what makes an edited revert's
    // reflog read `revert: Revert "…"` rather than `commit: …`.
    let reflog_action = std::env::var("GIT_REFLOG_ACTION").ok().filter(|a| !a.is_empty());
    let reflog_override: Option<String> = match &reflog_action {
        Some(action) => Some(format!("{action}: {subject}")),
        None if whence.is_cherry_pick() => Some(format!("commit (cherry-pick): {subject}")),
        None if whence.is_rebase() => Some(format!("commit (rebase): {subject}")),
        None => None,
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
            message.as_bytes().as_bstr(),
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
                    // `if (!reflog_msg) reflog_msg = "commit (amend)"`
                    // (builtin/commit.c:1854-1856): an amend takes the whence-derived
                    // wording from nowhere, so only `GIT_REFLOG_ACTION` displaces it.
                    message: match &reflog_action {
                        Some(action) => format!("{action}: {subject}").into(),
                        None => format!("commit (amend): {subject}").into(),
                    },
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
            message.as_bytes().as_bstr(),
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

    // ```c
    // run_commit_hook(use_editor, repo_get_index_file(the_repository),
    //                 NULL, "post-commit", NULL);
    // if (amend && !no_post_rewrite) {
    //         commit_post_rewrite(the_repository, current_head, &oid);
    // }
    // ```
    //
    // (builtin/commit.c:1966-1970.) The order is load-bearing and was inverted
    // here: a `post-commit` hook that reads `HEAD` and a `post-rewrite` hook that
    // consumes the old→new mapping can each observe what the other did, so
    // running them the other way round changes what both see.
    //
    // `post-commit` is a notification hook: it runs after the commit regardless of
    // `--no-verify`, and its exit status is ignored.
    let _ = crate::hooks::run(&repo, "post-commit", &[], None);

    // `--amend` rewrites a commit, so git notifies `post-rewrite` with the
    // `amend` mode and one `<old-sha1> SP <new-sha1>` line on stdin;
    // `--no-post-rewrite` suppresses it. Its exit status is ignored.
    if amend && post_rewrite {
        if let Some(prev) = head_tip {
            let payload = format!("{} {}\n", prev, commit_id.detach());
            let _ = crate::hooks::run(&repo, "post-rewrite", &["amend"], Some(payload.as_bytes()));
        }
    }

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

    // --- short-stat + summary --------------------------------------------
    // `log_tree_diff()` bails out on a commit with more than one parent unless a
    // combined-diff mode is asked for, which `print_commit_summary()` never does,
    // so a merge prints its headline and nothing else.
    if is_merge_commit {
        break 'summary;
    }
    // `print_commit_summary()` does not count tree entries: it hands the commit to
    // the ordinary revision/diff machinery (sequencer.c:1462-1474) with
    // `DIFF_FORMAT_SHORTSTAT | DIFF_FORMAT_SUMMARY`, `show_root_diff = 1`, and
    // `rev.diffopt.detect_rename = DIFF_DETECT_RENAME` (sequencer.c:1473), then
    // lets `log_tree_commit()` diff the commit against its **first parent** — or
    // against the empty tree when it has none, which is what `show_root_diff`
    // buys. So the block below is produced by this port's own `diff-tree`, the
    // engine `git diff --shortstat --summary -M` already agrees with stock on,
    // rather than by a walk written a second time here. Two things that walk got
    // wrong, both of them silent:
    //
    //   * With rename detection off it reported `git mv old new` as
    //     ` 2 files changed, 20 insertions(+), 20 deletions(-)` plus a
    //     create/delete pair, where stock prints ` 1 file changed, 0 insertions(+),
    //     0 deletions(-)` and ` rename old.txt => new.txt (100%)`.
    //   * Its line counts came from `gix`'s tree-diff statistics, which run both
    //     blobs through the `Mode::ToGit` conversion pipeline before diffing them.
    //     A commit that rewrites `a\nb\n` as `a\r\nb\r\nc\r\n` scored
    //     ` 1 insertion(+)` there — the CRLF was normalized away on both sides —
    //     against stock's ` 3 insertions(+), 2 deletions(-)`.
    //
    // `-r` is `log_tree_diff()`'s recursion, without which a change under `src/`
    // would be charged to the `src` tree object; the operands are the two trees
    // rather than the two commits so that `diff-tree` prints no commit-id header.
    let left = parent_tree_id.unwrap_or_else(|| gix::ObjectId::empty_tree(hash));
    super::diff_tree::diff_tree(&[
        "-r".to_string(),
        "-M".to_string(),
        "--shortstat".to_string(),
        "--summary".to_string(),
        left.to_string(),
        tree_id.to_string(),
    ])?;
    } // 'summary

    // The merge is concluded, so the worktree `merge --autostash` put aside comes
    // back — git's very last act in `cmd_commit`.
    apply_merge_autostash(&repo)?;

    Ok(ExitCode::SUCCESS)
}

/// `--pathspec-from-file=<file>` — the pathspec list read from a file, or from
/// stdin for `-`. A port of `parse_pathspec_file()` (pathspec.c), shared by every
/// verb that offers the option: `add`, `checkout`, `commit`, `rm` and `stash` all
/// call git's one function, so they call one here too.
///
/// Entries are separated by `NUL` with `--pathspec-file-nul` (`strbuf_getline_nul`)
/// and by newlines otherwise (`strbuf_getline`, which also drops a trailing `\r`).
/// In the line form only, a line that opens with `"` is C-unquoted.
///
/// A blank line is a real, empty entry, not a skipped one: git hands the list
/// straight to `parse_pathspec()`, which dies on the first empty element. That check
/// belongs here rather than in each caller because every caller reaches it through
/// this one function. It runs after the whole file is read, so a badly-quoted line
/// anywhere reports first — `unquote_c_style()` fails inside the read loop while
/// `parse_pathspec()` only sees the finished list.
pub(super) fn read_pathspec_file(src: &str, nul: bool) -> Result<Vec<String>> {
    let raw = if src == "-" {
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        // `parse_pathspec_file()` opens the list with `xfopen()`, whose read-mode
        // failure is `die_errno(_("could not open '%s' for reading"), path)`
        // (wrapper.c) — git's `fatal:` at 128, not this port's own `zvcs: <cmd>:`
        // line at 1, which is what a caller testing `$?` for 128 would miss.
        // `strerror()` has no Rust `(os error <n>)` tail, so the tail is trimmed.
        std::fs::read(src).map_err(|e| {
            let text = e.to_string();
            let reason = text.split(" (os error ").next().unwrap_or(&text);
            crate::fatal::die(format!("could not open '{src}' for reading: {reason}"))
        })?
    };
    let sep = if nul { b'\0' } else { b'\n' };
    let mut out = Vec::new();
    // `getline` yields a line per separator and one more for a final unterminated
    // tail, so a trailing separator closes the last line instead of opening an empty
    // one — and an empty file has no lines at all.
    let body = raw.strip_suffix(&[sep]).unwrap_or(&raw);
    if !raw.is_empty() {
        for chunk in body.split(|&b| b == sep) {
            let line = if nul { chunk } else { chunk.strip_suffix(b"\r").unwrap_or(chunk) };
            if !nul && line.first() == Some(&b'"') {
                match unquote_c_style(line) {
                    Some(v) => out.push(String::from_utf8_lossy(&v).into_owned()),
                    None => {
                        return Err(crate::fatal::die(format!(
                            "line is badly quoted: {}",
                            String::from_utf8_lossy(line)
                        )))
                    }
                }
            } else {
                out.push(String::from_utf8_lossy(line).into_owned());
            }
        }
    }
    if out.iter().any(String::is_empty) {
        return Err(crate::fatal::die(
            "empty string is not a valid pathspec. \
             please use . instead if you meant to match all paths",
        ));
    }
    Ok(out)
}

/// Port of `unquote_c_style()` (quote.c) for one double-quoted line; `None` is its
/// `-1`, which `parse_pathspec_file()` turns into `line is badly quoted: <line>`.
///
/// Everything up to the first unescaped `"` is the result and whatever follows the
/// closing quote is ignored, because git passes a NULL `endp` and never looks. The
/// octal escape is the strict `\NNN` form: exactly three digits, and a leading digit
/// above `3` is rejected rather than wrapped, since it would overflow a byte.
///
/// git walks a NUL-terminated string, so a read past the end lands on `\0`, which
/// no arm accepts — reading out of range as `0` here reproduces that exactly, and
/// an embedded NUL byte truncates the same way `strcspn()` does.
fn unquote_c_style(line: &[u8]) -> Option<Vec<u8>> {
    let at = |i: usize| line.get(i).copied().unwrap_or(0);
    if at(0) != b'"' {
        return None;
    }
    let mut i = 1;
    let mut out = Vec::with_capacity(line.len());
    loop {
        // `strcspn(quoted, "\"\\")`: copy through to the next delimiter.
        while !matches!(at(i), b'"' | b'\\' | 0) {
            out.push(at(i));
            i += 1;
        }
        let delim = at(i);
        i += 1;
        match delim {
            b'"' => return Some(out),
            b'\\' => {}
            _ => return None,
        }
        let esc = at(i);
        i += 1;
        let byte = match esc {
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 0x0b,
            b'\\' | b'"' => esc,
            b'0'..=b'3' => {
                let mut ac = (esc - b'0') << 6;
                for shift in [3, 0] {
                    let d = at(i);
                    i += 1;
                    if !d.is_ascii_digit() || d > b'7' {
                        return None;
                    }
                    ac |= (d - b'0') << shift;
                }
                ac
            }
            _ => return None,
        };
        out.push(byte);
    }
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
            crate::git_fatal!("Invalid untracked files mode '{u}'");
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
        // An as-is dry run is not read-only, and that is not an accident of this
        // port: `dry_run_commit()` calls `prepare_index(..., is_status=1)` and
        // `is_status` only widens the refresh flags (builtin/commit.c:365-366), so
        // the as-is branch still refreshes the index, updates its cache-tree and
        // writes it with `COMMIT_LOCK` (builtin/commit.c:482-491). The
        // `rollback_index_files()` that follows the report only rolls back locks
        // that were never committed, which is why `-a` and `-i` — whose prepared
        // index lives in a lock file — leave the real one alone while this does not.
        let mut real = open_or_empty_index(repo)?;
        super::write_tree::update_cache_tree_if_stale(repo, &mut real)?;
        None
    };

    // `run_status()`'s reference: `--amend` measures against `HEAD^1`, since the
    // commit it would write replaces `HEAD` rather than following it.
    let reference = match o.amend {
        true => super::status::Reference::AmendParent,
        false => super::status::Reference::Commit,
    };
    let committable = index_differs_from_reference(repo, prepared.as_ref(), reference)?;

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
    // `s->verbose = verbose` (builtin/commit.c:575): the dry run ends with the
    // staged patch, which the status engine already knows how to append.
    if o.verbose {
        sargs.push("--verbose".to_string());
    }

    match &prepared {
        Some(index) => {
            let _swap = IndexSwap::install(repo, index)?;
            super::status::status_with(&sargs, reference)?;
        }
        None => {
            super::status::status_with(&sargs, reference)?;
        }
    }

    Ok(if committable {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// The report the empty-commit refusal is made of: `run_status(stdout,
/// index_file, prefix, 0, s)` (builtin/commit.c:1085).
///
/// Same engine as [`dry_run_commit`], and for the same reason — git points one
/// `wt_status` at the index the commit would have used and prints it. No index is
/// installed here because by this point the port has already applied `-a`, `-i`,
/// `--only` and the interactive selection to the real one, which is what git's
/// prepared index holds.
///
/// `-u<mode>` reaches `wt_status` through `handle_untracked_files_arg()` before
/// the refusal, so it is forwarded. `-v` is not: git's `wt_longstatus_print()`
/// would append the staged diff, and there is by definition nothing staged.
fn report_nothing_to_commit(
    untracked: Option<&str>,
    reference: super::status::Reference,
) -> Result<()> {
    let mut sargs = vec!["--long".to_string()];
    if let Some(u) = untracked {
        sargs.push(format!("--untracked-files={u}"));
    }
    super::status::status_with(&sargs, reference)?;
    Ok(())
}

/// Whether the given index (or the on-disk one when `None`) records anything
/// different from the reference's tree — git's `wt_status.committable`, which
/// decides a dry run's exit status. An unmerged entry always counts as
/// committable.
///
/// The reference is `HEAD` for an ordinary commit and `HEAD^1` under `--amend`
/// (builtin/commit.c:1035-1036), which is the same split `run_status()` makes.
fn index_differs_from_reference(
    repo: &gix::Repository,
    index: Option<&gix::index::File>,
    reference: super::status::Reference,
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
    // `if (repo_get_oid(parent, &oid))` — an unresolvable parent (an amend of a
    // root commit, or a commit with no `HEAD` yet) makes git fall back to "is
    // there anything in the index at all", which the empty tree stands in for.
    let reference_tree = match repo.rev_parse_single(reference.spec()).ok() {
        Some(id) => Some(repo.find_commit(id.detach())?.tree_id()?.detach()),
        None => None,
    };
    let old = match reference_tree {
        Some(t) => flatten(&repo.index_from_tree(&t)?),
        None => Vec::new(),
    };
    Ok(flatten(index) != old)
}

/// git's `index.lock` around `commit --interactive` (`prepare_index()`,
/// builtin/commit.c:395).
///
/// git writes the current index into `index.lock`, points
/// `the_repository->index_file` and `GIT_INDEX_FILE` at that copy, lets the
/// selector stage into it, and reads the result back into its in-memory index.
/// `commit_index_files()` — reached once the commit object exists and `HEAD` has
/// moved — renames the copy over the real index; an aborted commit (empty
/// message, a failing `pre-commit`, an editor that exits non-zero) instead rolls
/// it back and the selection is discarded.
///
/// `commit -p` reproduces the scratch index. [`Self::staging_index`] is the path
/// [`super::add_patch::run_in_index`] exports as `GIT_INDEX_FILE` to the
/// selector's `apply --cached` children, so the repository's own index never
/// holds a half-finished selection — not even while the selector sits waiting on
/// the next keystroke, which under an interactive command can be a long time.
///
/// What is *not* reproduced is git's in-memory index, and that is what the swap
/// below is for. git can point `index_file` back at the real index and still
/// build the tree out of the scratch copy, because that content lives in
/// `the_repository->index`. Every read here goes to disk through
/// `repo.open_index()` — the tree build in [`commit`] among them — so the
/// prepared index has to *be* at `repo.index_path()` before the commit proceeds.
/// [`Self::adopt`] moves the original aside and renames the scratch index into
/// place as soon as the selector returns; [`Drop`] renames the original back
/// unless [`Self::keep`] has been called. Both end states — kept on success,
/// discarded on abort — are git's.
///
/// An earlier comment here justified the swap by claiming this build ignores
/// `GIT_INDEX_FILE`. It does not: `gix::Repository::index_path()` reads the
/// variable, which is exactly how the scratch index above works and how
/// `git history split` and `git stash -p` drive the same selector.
///
/// `commit --interactive` — the numbered menu rather than `-p` — has no scratch
/// index, because [`super::add_interactive`] reads and writes `repo.index_path()`
/// itself instead of shelling out with `GIT_INDEX_FILE` set, so there is nothing
/// to point elsewhere. It stages into the repository's index directly and only
/// the rollback half applies, which is why [`Self::hold`] copies the original
/// aside for that mode instead of seeding a scratch index.
struct InteractiveStage {
    /// The repository index the prepared selection has to end up at.
    index: std::path::PathBuf,
    /// The scratch index the `-p` selector stages into, until [`Self::adopt`]
    /// renames it over [`Self::index`]. `None` for the numbered menu, which
    /// stages into the repository's index directly.
    scratch: Option<std::path::PathBuf>,
    /// Where the index as it was before the selector ran is parked, or `None`
    /// when the repository had no index file at all.
    original: Option<std::path::PathBuf>,
    /// Set once the commit has succeeded, which disarms the rollback.
    keep: bool,
}

/// The scratch index `commit -p` stages into, and where the original is parked
/// for the duration. Both sit beside the index so that moving one into place is a
/// rename on the same filesystem rather than a copy.
const SCRATCH_INDEX: &str = "index.zvcs-interactive";
const ORIGINAL_INDEX: &str = "index.zvcs-interactive-orig";

impl InteractiveStage {
    /// `patch` picks git's scratch-index arrangement; the numbered menu gets the
    /// copy-aside fallback described on the type.
    fn hold(repo: &gix::Repository, patch: bool) -> Result<Self> {
        let index = repo.index_path();
        let exists = index.exists();
        let mut guard = Self { index, scratch: None, original: None, keep: false };
        if patch {
            let scratch = guard.index.with_file_name(SCRATCH_INDEX);
            // A scratch index left behind by a killed run must not be inherited.
            let _ = std::fs::remove_file(&scratch);
            if exists {
                std::fs::copy(&guard.index, &scratch)?;
            }
            guard.scratch = Some(scratch);
        } else if exists {
            let original = guard.index.with_file_name(ORIGINAL_INDEX);
            let _ = std::fs::remove_file(&original);
            std::fs::copy(&guard.index, &original)?;
            guard.original = Some(original);
        }
        Ok(guard)
    }

    /// The index the selector stages into: the scratch copy under `-p`, the
    /// repository's own under the numbered menu.
    fn staging_index(&self) -> &std::path::Path {
        self.scratch.as_deref().unwrap_or(&self.index)
    }

    /// git's `read_index_from(get_lock_file_path(&index_lock))`: the selection
    /// becomes the index the rest of the commit reads. The original is *moved*
    /// aside rather than copied, so a rollback brings it back with its inode,
    /// mode and mtime intact.
    ///
    /// A no-op for the numbered menu, which has already staged into `index`.
    fn adopt(&mut self) -> Result<()> {
        let Some(scratch) = self.scratch.take() else { return Ok(()) };
        if self.index.exists() {
            let original = self.index.with_file_name(ORIGINAL_INDEX);
            let _ = std::fs::remove_file(&original);
            std::fs::rename(&self.index, &original)?;
            self.original = Some(original);
        }
        // The selector only writes the scratch index when something was staged,
        // so a run that selected nothing in a repository that had no index leaves
        // none behind — which is what git's empty prepared index amounts to.
        if scratch.exists() {
            std::fs::rename(&scratch, &self.index)?;
        }
        Ok(())
    }

    /// git's `commit_index_files()`: the staged selection stands.
    fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for InteractiveStage {
    fn drop(&mut self) {
        // Reached only when the selector itself failed, since `adopt` takes it.
        if let Some(scratch) = &self.scratch {
            let _ = std::fs::remove_file(scratch);
        }
        match &self.original {
            Some(original) if self.keep => {
                let _ = std::fs::remove_file(original);
            }
            Some(original) => {
                let _ = std::fs::rename(original, &self.index);
            }
            // There was no index before the selector ran, so git's rollback leaves
            // the repository without one.
            None if !self.keep => {
                let _ = std::fs::remove_file(&self.index);
            }
            None => {}
        }
    }
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
        // The dry-run swap stands in for git's partial-commit `false_lock`
        // (`next-index-<pid>`, builtin/commit.c:541-550), which is written by the
        // same `write_locked_index()` -> `do_write_index()` pair as the real
        // index — `skip_hash` and all (read-cache.c:2830-2831). A hook that reads
        // this index through `GIT_INDEX_FILE` therefore sees the trailer the
        // repository asked for, not the one this code path felt like writing.
        prepared.write_to(&mut bytes, crate::config::index_write_options(repo))?;
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
/// The status body is the whole of `wt_status_print()` — see
/// [`super::status::commit_template_block`] for the settings it runs under.
fn editor_status_block(
    repo: &gix::Repository,
    comment: &str,
    cleanup: Cleanup,
    whence: Whence,
    // The identity the commit will record, which the block names whenever it
    // differs from the committer's.
    author: &gix::actor::Signature,
    // `author_date_is_interesting()` (builtin/commit.c:694): the author date is
    // shown when it was inherited or forced rather than taken from the clock.
    date_is_interesting: bool,
    // `-u<mode>`, which reached `wt_status` before `prepare_to_commit()` ran.
    untracked: Option<&str>,
    // `s->reference` / `s->amend`, as `run_status()` sets them.
    reference: super::status::Reference,
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
    // The three conditional identity lines (builtin/commit.c:998-1019). Each is a
    // `status_printf_ln` whose leading `%s` is `"\n"` for the *first* one shown
    // and `""` afterwards, so the group opens with a bare comment line and the
    // whole group vanishes when none of the three applies.
    let committer = repo
        .committer()
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("unable to determine committer identity"))?
        .to_owned()?;
    let mut shown = false;
    let mut ident_line = |buf: &mut String, body: String| {
        if !shown {
            buf.push_str(&format!("{comment}\n"));
            shown = true;
        }
        buf.push_str(&format!("{comment} {body}\n"));
    };
    // `ident_cmp()` (ident.c:724-736) compares the address first and the name
    // second; the date is no part of it.
    if (&author.email, &author.name) != (&committer.email, &committer.name) {
        ident_line(
            &mut buf,
            format!("Author:    {} <{}>", author.name, author.email),
        );
    }
    // `show_ident_date(&ai, DATE_MODE(NORMAL))`.
    if date_is_interesting {
        ident_line(
            &mut buf,
            format!(
                "Date:      {}",
                author.time.format_or_unix(gix::date::time::format::DEFAULT)
            ),
        );
    }
    if !committer_ident_sufficiently_given(&repo.config_snapshot()) {
        ident_line(
            &mut buf,
            format!("Committer: {} <{}>", committer.name, committer.email),
        );
    }
    // `status_printf_ln(s, GIT_COLOR_NORMAL, "%s", "")` — "Add new line for
    // clarity" (builtin/commit.c:1021).
    buf.push_str(&format!("{comment}\n"));
    // `run_status(s->fp, index_file, prefix, 1, s)` (builtin/commit.c:1025): the
    // whole `wt_status_print()` body, commented, uncolored and hintless. Its own
    // closing section trailer is the block's last line.
    buf.push_str(&super::status::commit_template_block(
        reference, untracked, comment,
    )?);
    Ok(buf)
}

/// git's `committer_ident_sufficiently_given()` (ident.c:600-603): whether the
/// committer's *address* was given rather than worked out from the machine. The
/// editor block names the committer only when it was not (builtin/commit.c:1013),
/// which is how a first-time user is told what is about to be recorded.
///
/// `IDENT_MAIL_GIVEN` comes from `GIT_COMMITTER_EMAIL` (ident.c:582-583), from
/// `committer.email` / `user.email` in the config (ident.c:645-648, :663-666),
/// and from `EMAIL` when `ident_default_email()` fell back to it (ident.c:176-179).
/// A hostname, `/etc/mailname` or passwd guess is none of those.
fn committer_ident_sufficiently_given(snap: &gix::config::Snapshot<'_>) -> bool {
    let env = |key: &str| std::env::var_os(key).is_some_and(|v| !v.is_empty());
    let cfg = |key: &str| snap.string(key).is_some_and(|v| !v.is_empty());
    env("GIT_COMMITTER_EMAIL") || cfg("committer.email") || cfg("user.email") || env("EMAIL")
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
        Some(other) => crate::git_fatal!("Invalid cleanup mode {other}"),
    })
}

/// Write the commit object, optionally carrying a `gpgsig` header.
///
/// git signs the *unsigned* serialization and then inserts the detached signature
/// as an extra header, which is exactly what happens here: the object is encoded
/// once without the header, handed to `sign_buffer()`, and re-encoded with
/// `gpgsig` first among the extra headers — the slot git writes it in. What the
/// signature looks like is the backend's business: armored PGP for
/// `openpgp`/`x509`, an `SSH SIGNATURE` block for `ssh`.
pub(crate) fn write_commit_object(
    repo: &gix::Repository,
    committer: &gix::actor::Signature,
    author: &gix::actor::Signature,
    // A commit message is bytes, not text: `--file`, a picked commit's own
    // message and a `-m` argument can all be non-UTF-8, and re-encoding one on
    // the way in would change the object.
    message: &gix::bstr::BStr,
    tree: ObjectId,
    parents: Vec<ObjectId>,
    signer: Option<&crate::gitsig::Signer>,
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
        // Both backends already carry the whole text of git's own diagnostic —
        // `sign_buffer_gpg`'s `gpg failed to sign the data:` wrapper around gpg's
        // status stream (gpg-interface.c:1045) and `sign_buffer_ssh`'s verbatim
        // relay of ssh-keygen's stderr (gpg-interface.c:1125) — so all that is
        // left here is the `error: ` prefix and the `die()` on top, which is
        // `commit_tree_extended`'s failure: `fatal:` at 128, not this port's own
        // voice at 1. A `Fatal` came from `get_signing_key()` instead, which dies
        // on the spot with nothing after it.
        let sig = s.sign(&payload).map_err(|e| match e {
            // Reported in full already (an invalid `gpg.format`); only the exit
            // code is left, and `commit_tree_extended`'s `die()` never runs
            // because git stopped inside the config reader instead.
            crate::gitsig::SignFailure::Silent => {
                anyhow::Error::new(crate::fatal::Silent(crate::fatal::EXIT_FATAL))
            }
            crate::gitsig::SignFailure::Fatal(m) => crate::fatal::die(m),
            crate::gitsig::SignFailure::Error(m) => {
                eprintln!("{}", crate::gitsig::report("error: ", &m));
                crate::fatal::die("failed to write commit object")
            }
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
/// them. Returns the tree id the commit is written against.
///
/// A pathspec matches a path when the path equals it or lives under `<spec>/`;
/// literal files and directory prefixes are supported, as are the worktree globs
/// the dirwalk resolves. Blob hashing and mode detection mirror `git add`.
fn build_only_mode_tree(repo: &gix::Repository, pathspecs: &[String]) -> Result<ObjectId> {
    // The commit tree comes from git's "false index" — HEAD's tree with only the
    // matched paths taken from the worktree. The same staged set is then applied
    // to the real index, so the worktree is walked and hashed exactly once.
    let (temp, staged) = only_mode_stage(repo, pathspecs)?;

    let hash = repo.object_hash();
    let mut editor = gix::objs::tree::Editor::new(gix::objs::Tree::empty(), &repo.objects, hash);
    {
        let backing = temp.path_backing();
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
        }
    }
    let tree_id = editor.write(|tree| repo.write_object(tree).map(|id| id.detach()))?;

    // Stage the same paths into the REAL on-disk index, leaving all other entries
    // — git's step (2)/(3), which is what makes the partial commit visible to the
    // next one.
    let mut real = open_or_empty_index(repo)?;
    staged.apply_to(&mut real);
    // Step (2)/(3) (builtin/commit.c:534-539) rewrites the real on-disk index,
    // so it carries the
    // repository's index-write options like every other write does
    // (read-cache.c:2830-2831). The `cache_tree_update()` that git runs between
    // the staging and the write (builtin/commit.c:537) is what leaves the real
    // index with a *valid* cache-tree afterwards rather than the invalidated one
    // `apply_to` produced — the partial commit's whole point is that the next
    // command sees a ready index.
    super::write_tree::update_cache_tree_quietly(repo, &mut real);
    real.write(crate::config::index_write_options(repo))?;

    Ok(tree_id)
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
    ///
    /// Each touched path is invalidated in the tree-cache, which is what git does
    /// from inside `add_index_entry_with_check()` (read-cache.c:1273-1274) and
    /// `remove_file_from_index()` (read-cache.c:632) for every single entry it
    /// adds or drops. Directories no path here touches keep their cached tree ids,
    /// so the `cache_tree_update()` that follows only re-serialises what moved.
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
        for path in &remove {
            index.invalidate_path_in_tree(path.as_ref());
        }
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
///
/// A matched path whose worktree entry is a *directory* is a submodule gitlink and
/// is staged as mode 160000 from that submodule's checked-out HEAD, which is what
/// git's `add_to_index()` does for `S_ISDIR` via `index_path()`'s
/// `resolve_gitlink_ref()`.
fn stage_pathspecs(
    repo: &gix::Repository,
    pathspecs: &[String],
    tracked: &HashMap<BString, (ObjectId, Mode)>,
    known: &HashSet<BString>,
) -> Result<StagedSet> {
    if repo.workdir().is_none() {
        crate::git_fatal!("this operation must be run in a work tree");
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

    // --- submodule gitlinks: record the submodule's checked-out HEAD (mode 160000)
    // The walk above yields only blobs and symlinks — a submodule worktree comes out
    // as `Kind::Repository` and is dropped there — so a gitlink would never reach the
    // partial commit's tree, and `git commit -- <submodule>` silently committed
    // nothing. git's `add_remove_files()` lstats every path `list_paths()` matched and
    // hands a directory to `add_to_index()`, which stores the submodule's HEAD
    // (`index_path()` → `resolve_gitlink_ref()`), *not* the value already staged in the
    // index. Driving this off `known` — the index overlaid with HEAD for `--only`, the
    // index alone for `-i` — is exactly the set `list_paths()` yields.
    //
    // A directory git cannot resolve a HEAD for (an uninitialized submodule, a plain
    // directory) is not an error: `ce_compare_gitlink()` reports an unresolvable
    // gitlink as unchanged, so the entry is left alone — neither restaged here nor
    // treated as a deletion below, since the directory does exist.
    for path in known {
        if staged_set.contains(path) || !pathspec.is_included(path.as_bstr(), Some(false)) {
            continue;
        }
        let Some(abs) = repo.workdir_path(path.as_bstr()) else {
            continue;
        };
        let Ok(md) = gix::index::fs::Metadata::from_path_no_follow(&abs) else {
            continue; // vanished — the deletion pass below owns this path
        };
        if !md.is_dir() {
            continue;
        }
        let Some(id) = gix::open(&abs)
            .ok()
            .and_then(|sub| sub.head_id().ok().map(|h| h.detach()))
        else {
            continue;
        };
        staged_set.insert(path.clone());
        staged.push(StagedFile {
            path: path.clone(),
            id,
            mode: Mode::COMMIT,
            stat: Stat::from_fs(&md).unwrap_or_default(),
        });
    }

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
            // `report_path_error()` writes `error:` and the caller exits 1 — this
            // is not a `die()`, so it is neither `fatal:` nor 128.
            eprintln!("error: pathspec '{p}' did not match any file(s) known to git");
            return Err(anyhow::Error::new(crate::fatal::Silent(1)));
        }
    }

    Ok(StagedSet { staged, deletions })
}

/// Stage every *tracked* path whose worktree state diverges from the index —
/// `git commit -a`, which is `git add -u` over the whole worktree.
///
/// Only stage-0 entries participate: conflicted stages are left for the caller's
/// unmerged-files check to reject. Submodule gitlinks move to the submodule's
/// checked-out HEAD, which is what git's `-a` does (`add_files_to_cache()` diffs
/// with `ignore_submodule_ignore_config`, so a moved pointer shows up as modified
/// and `add_file_to_index()` re-resolves it). Untracked files are deliberately not
/// added, which is the whole distinction between `-a` and `git add -A`.
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
    // `add_files_to_cache()` + `write_locked_index()` (builtin/commit.c:454-465):
    // an ordinary index write, with the repository's options
    // (read-cache.c:2830-2831).
    index.write(crate::config::index_write_options(repo))?;
    Ok(())
}

/// The `-a`/`--all` scan itself: every stage-0 index entry whose worktree content,
/// mode or (for a gitlink) submodule HEAD moved, plus the tracked paths that
/// vanished. Split out from [`stage_tracked_changes`] so `--dry-run -a` can build
/// the prepared index without writing it.
fn collect_tracked_changes(
    repo: &gix::Repository,
    index: &gix::index::File,
) -> Result<StagedSet> {
    if repo.workdir().is_none() {
        crate::git_fatal!("this operation must be run in a work tree");
    }
    let mut staged: Vec<StagedFile> = Vec::new();
    let mut deletions: Vec<BString> = Vec::new();

    {
        let backing = index.path_backing();
        for e in index.entries() {
            if e.stage() != Stage::Unconflicted {
                continue;
            }
            let path = e.path_in(backing).to_owned();
            let Some(abs) = repo.workdir_path(&path) else {
                continue;
            };
            // A vanished (or unreadable) tracked path stages as a deletion — a
            // submodule whose whole worktree is gone included.
            let Ok(md) = gix::index::fs::Metadata::from_path_no_follow(&abs) else {
                deletions.push(path);
                continue;
            };
            // A directory is a submodule: stage its checked-out HEAD as the gitlink.
            // One that has no resolvable HEAD (uninitialized submodule, or a tracked
            // file replaced by a plain directory) is left untouched, matching
            // `ce_compare_gitlink()`'s "unresolvable reads as unchanged".
            if md.is_dir() {
                let Some(id) = gix::open(&abs)
                    .ok()
                    .and_then(|sub| sub.head_id().ok().map(|h| h.detach()))
                else {
                    continue;
                };
                if id == e.id && e.mode == Mode::COMMIT {
                    continue;
                }
                staged.push(StagedFile {
                    path,
                    id,
                    mode: Mode::COMMIT,
                    stat: Stat::from_fs(&md).unwrap_or_default(),
                });
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

/// The comment prefix: `core.commentChar` / `core.commentString`, which are one
/// knob in git 2.55, so the *last* of the two set anywhere in the merged
/// configuration wins. `auto` and an empty value give `#`.
///
/// This is the same value the trailer scan runs on, read through the same
/// function, so a message can never be cleaned with one prefix and scanned for
/// trailers with another.
pub(super) fn comment_prefix(snap: &gix::config::Snapshot<'_>) -> String {
    comment_prefix_full(snap).0
}

/// [`comment_prefix`] plus git's `auto_comment_line_char` flag, for the one
/// caller that has to know: `prepare_to_commit()` re-picks the character against
/// the message body when `auto` is configured.
fn comment_prefix_full(snap: &gix::config::Snapshot<'_>) -> (String, bool) {
    let (bytes, auto) = super::interpret_trailers::comment_string_full(snap.plumbing());
    (String::from_utf8_lossy(&bytes).into_owned(), auto)
}

/// The candidate list `adjust_comment_line_char()` picks from, in git's order.
const COMMENT_CHAR_CANDIDATES: &[u8] = b"#;@!$%^&|:";

/// ```c
/// static void adjust_comment_line_char(const struct strbuf *sb)
/// {
///         char candidates[] = "#;@!$%^&|:";
///         ...
///         cutoff = sb->len - ignored_log_message_bytes(sb->buf, sb->len);
///         if (!memchr(sb->buf, candidates[0], sb->len)) { comment_line_str = "#"; return; }
///         p = sb->buf;
///         candidate = strchr(candidates, *p);
///         if (candidate) *candidate = ' ';
///         for (p = sb->buf; p + 1 < sb->buf + cutoff; p++)
///                 if ((p[0] == '\n' || p[0] == '\r') && p[1]) {
///                         candidate = strchr(candidates, p[1]);
///                         if (candidate) *candidate = ' ';
///                 }
///         for (p = candidates; *p == ' '; p++) ;
///         if (!*p) die(_("unable to select a comment character that is not used\n"
///                        "in the current commit message"));
///         comment_line_str = xstrfmt("%c", *p);
/// }
/// ```
///
/// (builtin/commit.c:700-736.) Under `core.commentChar = auto` this runs against
/// the message body — after `-s` has appended its trailer, before the status
/// block is built — and moves the comment character off any candidate the body
/// already starts a line with. Without it a body line beginning with `#` is
/// treated as a comment and **deleted** by the `strip` cleanup an editor implies:
/// content lost at exit 0 with no diagnostic.
///
/// Three details that are easy to get wrong. The early return tests for `#`
/// anywhere in the whole buffer, not only at a line start and not bounded by
/// `cutoff` — a `#` in the middle of a line is enough to trigger the search. The
/// very first byte of the buffer counts as a line start even though the loop
/// below only looks after `\n`/`\r`. And `cutoff` excludes the trailing comment
/// run `ignored_log_message_bytes()` finds, so a `# Conflicts:` block left by a
/// stopped merge does not rule its own character out.
fn adjust_comment_line_char(body: &str) -> Result<String> {
    let buf = body.as_bytes();
    if !buf.contains(&COMMENT_CHAR_CANDIDATES[0]) {
        return Ok((COMMENT_CHAR_CANDIDATES[0] as char).to_string());
    }
    let cutoff = buf.len() - ignore_non_trailer(buf);
    let mut candidates = COMMENT_CHAR_CANDIDATES.to_vec();
    let mut strike = |c: u8| {
        if let Some(i) = candidates.iter().position(|&x| x == c) {
            candidates[i] = b' ';
        }
    };
    if let Some(&first) = buf.first() {
        strike(first);
    }
    for i in 0..cutoff.saturating_sub(1) {
        if (buf[i] == b'\n' || buf[i] == b'\r') && buf[i + 1] != 0 {
            strike(buf[i + 1]);
        }
    }
    match candidates.iter().find(|&&c| c != b' ') {
        Some(&c) => Ok((c as char).to_string()),
        None => Err(crate::fatal::die(
            "unable to select a comment character that is not used\n\
             in the current commit message",
        )),
    }
}

/// `git_editor()` (editor.c:27-46), which is finickier than it looks:
///
/// ```c
/// const char *editor = getenv("GIT_EDITOR");
/// int terminal_is_dumb = is_terminal_dumb();
///
/// if (!editor && editor_program)      editor = editor_program;
/// if (!editor && !terminal_is_dumb)   editor = getenv("VISUAL");
/// if (!editor)                        editor = getenv("EDITOR");
/// if (!editor && terminal_is_dumb)    return NULL;
/// if (!editor)                        editor = DEFAULT_EDITOR;
/// ```
///
/// Three details this used to get wrong. `getenv` returns non-NULL for an *empty*
/// variable, so `GIT_EDITOR=` selects the empty editor and fails at the exec —
/// it does not fall through to `core.editor`. `$VISUAL` is skipped on a dumb
/// terminal but `$EDITOR` is not. And the only thing that makes git give up is a
/// dumb `TERM`: whether stdin is a terminal never enters into it, so a redirected
/// stdin still gets `vi`.
///
/// `None` is git's NULL, which `launch_specified_editor()` reports as
/// "Terminal is dumb, but EDITOR unset".
fn resolve_editor(snap: &gix::config::Snapshot<'_>) -> Option<String> {
    let dumb = is_terminal_dumb();
    if let Some(e) = std::env::var("GIT_EDITOR").ok() {
        return Some(e);
    }
    if let Some(e) = snap.string("core.editor") {
        return Some(e.to_string());
    }
    if !dumb {
        if let Ok(e) = std::env::var("VISUAL") {
            return Some(e);
        }
    }
    if let Ok(e) = std::env::var("EDITOR") {
        return Some(e);
    }
    if dumb {
        return None;
    }
    Some("vi".to_string())
}

/// `is_terminal_dumb()` (editor.c:21-25): an unset `TERM` counts as dumb.
fn is_terminal_dumb() -> bool {
    std::env::var("TERM").map(|t| t == "dumb").unwrap_or(true)
}

/// Open `path` in the configured editor and wait, git-style: the editor string
/// runs through the shell so `core.editor = "code -w"` and other argument-bearing
/// commands work, and stdio is inherited so the interactive editor owns the tty.
pub(super) fn launch_editor(snap: &gix::config::Snapshot<'_>, path: &std::path::Path) -> Result<()> {
    // Every failure below is `error()` in `launch_specified_editor()`, not
    // `die()`: the message goes to stderr with an `error: ` prefix and the
    // *caller* decides the exit status (`builtin/commit.c:1124-1127` prints
    // "Please supply the message…" and exits 1). Wearing `fatal:`/128 here would
    // both misreport the code and claim git's voice for a line git never says.
    let Some(editor) = resolve_editor(snap) else {
        eprintln!("error: Terminal is dumb, but EDITOR unset");
        return Err(anyhow::Error::new(crate::fatal::Silent(1)));
    };
    // `if (strcmp(editor, ":"))` (editor.c:66): git's documented no-op editor is
    // recognised before any child is built, so nothing is spawned and not even
    // the "Waiting for your editor" hint is printed.
    if editor == ":" {
        return Ok(());
    }
    // `launch_specified_editor` (editor.c): when stderr is a terminal and
    // `advice.waitingForEditor` is on, git says why it is blocked before handing
    // the tty over. A dumb terminal cannot erase the line afterwards, so it gets
    // a newline instead of the erase sequence. The hint is never printed when
    // stderr is redirected, which is why scripted runs see none of this.
    let waiting = std::io::IsTerminal::is_terminal(&std::io::stderr())
        && crate::advice::Advice::WaitingForEditor.enabled();
    let dumb = is_terminal_dumb();
    if waiting {
        use std::io::Write;
        let tail = if dumb { "\n" } else { " " };
        eprint!("hint: Waiting for your editor to close the file...{tail}");
        let _ = std::io::stderr().flush();
    }
    // `start_command()` reports its own `cannot run <cmd>: <strerror>` before
    // `launch_specified_editor` adds `unable to start editor '<editor>'`
    // (editor.c:95-98), so a failed spawn produces two `error:` lines.
    let spawned = crate::external::prepare_shell_cmd_str(&editor, [path]).status();
    let status = match spawned {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot run {editor}: {}", crate::external::strerror(&e));
            eprintln!("error: unable to start editor '{editor}'");
            return Err(anyhow::Error::new(crate::fatal::Silent(1)));
        }
    };
    // `term_clear_line()`: wipe the "Waiting for your editor" line so the
    // command's real output starts on a clean line.
    if waiting && !dumb {
        use std::io::Write;
        eprint!("\r\x1b[K");
        let _ = std::io::stderr().flush();
    }
    if !status.success() {
        eprintln!("error: there was a problem with the editor '{editor}'");
        return Err(anyhow::Error::new(crate::fatal::Silent(1)));
    }
    Ok(())
}

/// git's `commit_msg_cleanup_mode` (builtin/commit.c), resolved by
/// [`resolve_cleanup`] from `--cleanup`/`commit.cleanup` and whether an editor
/// is used.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Cleanup {
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
pub(super) fn cleanup_message(raw: &str, comment: &str, mode: Cleanup, verbose: bool) -> String {
    // `strbuf_setlen(msg, wt_status_locate_end(...))` — the cut line and
    // everything below it never reach the commit.
    let raw = if verbose || mode == Cleanup::Scissors {
        let bytes = raw.as_bytes();
        let cend = super::interpret_trailers::c_len(bytes);
        &raw[..super::interpret_trailers::locate_end(bytes, bytes.len(), cend, comment.as_bytes())]
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
/// of `append_signoff()` (sequencer.c). The trailer is merged into an existing
/// trailer block, or set off by a blank line after a message body, and is
/// skipped when it is already the last trailer.
///
/// `ignore_footer` is the trailing run `commit` keeps below the trailer block
/// (`ignore_non_trailer()`); `format-patch` passes `0`, because the buffer it
/// hands over is the message alone.
///
/// `dedup` is git's `APPEND_SIGNOFF_DEDUP`: `commit` passes `flag == 0` and so
/// re-appends a trailer that appears earlier in the block, while `format-patch`
/// passes the flag and leaves the block alone.
pub(crate) fn append_signoff(msg: &mut String, ident: &str, ignore_footer: usize, dedup: bool) {
    // `ensure_configured()`: the trailer scan below reads the comment prefix,
    // `trailer.separators` and every configured `trailer.<token>.key`.
    let cfg = trailer_config();
    let sob = format!("Signed-off-by: {ident}\n");
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
        has_conforming_footer(&msg.as_bytes()[..cut], sob_bytes, cfg)
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
    if has_footer != 3 && (!dedup || has_footer != 2) {
        let pos = msg.len() - ignore_footer;
        msg.insert_str(pos, &sob);
    }
}

/// Port of `has_conforming_footer()` (sequencer.c) for the default `flag == 0`
/// path: returns `0` when the tail has no trailer block, `3` when `sob` is the
/// last trailer, `2` when `sob` appears earlier, `1` otherwise. `sub` is the
/// message truncated to `len - ignore_footer`, which is how git NUL-terminates
/// it before handing it to `trailer_info_get()`.
///
/// The block itself comes from `interpret-trailers`' [`block_get`], the same
/// `trailer_info_get()` port git shares between the two commands — so the
/// comment prefix, the `trailer.separators` and every configured
/// `trailer.<token>.key` are honoured here exactly as they are there.
///
/// [`block_get`]: super::interpret_trailers::block_get
fn has_conforming_footer(sub: &[u8], sob: &[u8], cfg: &TrailerConfig) -> u8 {
    // `opts.no_divider = 1`: the caller already cut the buffer where it wants.
    let block = super::interpret_trailers::block_get(sub, true, cfg);
    if block.start == block.end {
        return 0;
    }
    let mut found_sob = false;
    let mut found_sob_last = false;
    let last_idx = block.lines.len().wrapping_sub(1);
    for (i, line) in block.lines.iter().enumerate() {
        if line.starts_with(sob) {
            found_sob = true;
            if i == last_idx {
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

/// Port of `ignore_non_trailer()` (commit.c): the number of trailing bytes to
/// ignore — a run of comment/blank lines (and an old `Conflicts:` block) at the
/// very end, or everything past a `>8` scissors line.
///
/// git's copy in `commit.c` and the one `trailer.c` uses are the same routine
/// over the same `comment_line_str`, so this defers to the shared body rather
/// than keeping a second one that could only drift from it.
pub(crate) fn ignore_non_trailer(buf: &[u8]) -> usize {
    let cfg = trailer_config();
    super::interpret_trailers::ignored_log_message_bytes(
        buf,
        buf.len(),
        super::interpret_trailers::c_len(buf),
        cfg,
    )
}

/// The trailer configuration, read once per process.
///
/// This is `ensure_configured()`'s `static int configured`: the sign-off path
/// asks for it twice (once to find the tail to keep, once to scan the block),
/// and git reads the configuration for the first of those only. It cannot fail
/// either — a configuration git cannot parse has already aborted the command
/// long before a trailer is looked at.
fn trailer_config() -> &'static TrailerConfig {
    static CONFIGURED: std::sync::OnceLock<TrailerConfig> = std::sync::OnceLock::new();
    CONFIGURED
        .get_or_init(|| super::interpret_trailers::load_config().unwrap_or_default())
}

#[cfg(test)]
mod pathspec_file_tests {
    use super::{read_pathspec_file, unquote_c_style};

    fn unquoted(line: &str) -> Option<String> {
        unquote_c_style(line.as_bytes()).map(|v| String::from_utf8(v).expect("ascii fixtures"))
    }

    /// `parse_pathspec_file()` C-unquotes a line that opens with `"`, and the
    /// decoding stops at the closing quote because git passes a NULL `endp` and
    /// never looks at what follows. Verified against stock git 2.55.0, where a file
    /// holding `"a b"junk` stages `a b`.
    #[test]
    fn a_quoted_line_decodes_and_ignores_its_tail() {
        assert_eq!(unquoted(r#""a b""#).as_deref(), Some("a b"));
        assert_eq!(unquoted(r#""a b"junk"#).as_deref(), Some("a b"));
        assert_eq!(unquoted(r#""tab\there""#).as_deref(), Some("tab\there"));
        assert_eq!(unquoted(r#""q\"q""#).as_deref(), Some("q\"q"));
    }

    /// The octal escape is the strict three-digit form with a leading digit of `0`
    /// through `3`; every other shape is `-1`, which the caller turns into
    /// `line is badly quoted`. Stock git 2.55.0 accepts `"\101"` and refuses `"\1"`,
    /// `"\41"` and `"\501"` — the last because a first digit above `3` would
    /// overflow the byte. A port that accepted one or two digits, or any first digit
    /// up to `7`, silently invented pathspecs git rejects.
    #[test]
    fn the_octal_escape_is_exactly_three_digits_and_fits_a_byte() {
        assert_eq!(unquoted(r#""\101""#).as_deref(), Some("A"));
        assert_eq!(unquoted(r#""\1""#), None);
        assert_eq!(unquoted(r#""\41""#), None);
        assert_eq!(unquoted(r#""\501""#), None);
    }

    /// The other `goto error` paths: an unterminated line, an escape git has no arm
    /// for, and a trailing backslash that runs off the end of the string. git walks a
    /// NUL-terminated buffer, so running off the end lands on a byte no arm accepts.
    #[test]
    fn a_malformed_quoting_is_refused_rather_than_guessed() {
        assert_eq!(unquoted(r#""unterminated"#), None);
        assert_eq!(unquoted(r#""bad\q""#), None);
        assert_eq!(unquoted(r#""trailing\"#), None);
        // Not quoted at all: the caller only asks about lines opening with `"`.
        assert_eq!(unquoted("plain"), None);
    }

    /// `strbuf_getline` yields one line per separator plus a final unterminated tail,
    /// so a trailing newline closes the last line instead of opening an empty one and
    /// an empty file has no lines at all. An interior blank line *is* an entry, and
    /// `parse_pathspec()` dies on it — which is why `read_pathspec_file` carries that
    /// check rather than each of its five callers. Verified against stock git 2.55.0:
    /// `printf 'A\n'` stages `A`, `printf 'A\n\n'` dies, an empty file is a no-op.
    #[test]
    fn blank_lines_are_entries_but_a_trailing_separator_is_not() {
        let dir = std::env::temp_dir().join(format!("zvcs-pathspec-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        let read = |body: &str| {
            let path = dir.join("list");
            std::fs::write(&path, body).expect("scratch file");
            read_pathspec_file(&path.to_string_lossy(), false)
        };

        assert_eq!(read("A\n").expect("one entry"), vec!["A".to_string()]);
        assert_eq!(read("A").expect("one entry"), vec!["A".to_string()]);
        assert_eq!(read("").expect("no entries"), Vec::<String>::new());
        assert_eq!(
            read("A\n\n").unwrap_err().to_string(),
            "empty string is not a valid pathspec. \
             please use . instead if you meant to match all paths"
        );
        // A badly quoted line fails inside the read loop, so it reports before the
        // empty-entry check ever sees the finished list.
        assert_eq!(
            read("\n\"bad\n").unwrap_err().to_string(),
            r#"line is badly quoted: "bad"#
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing file is `xfopen()`'s `die_errno()`, which is git's `fatal:` at 128
    /// rather than this port's own `zvcs: <cmd>:` line at 1 — and `strerror()` has no
    /// Rust `(os error <n>)` tail to carry.
    #[test]
    fn a_missing_file_reports_the_way_git_does() {
        let missing = std::env::temp_dir()
            .join(format!("zvcs-absent-{}", std::process::id()))
            .join("nope");
        let err = read_pathspec_file(&missing.to_string_lossy(), false).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("could not open '{}' for reading: No such file or directory", missing.display())
        );
        assert!(err.downcast_ref::<crate::fatal::Fatal>().is_some(), "carries git's exit 128");
    }
}
