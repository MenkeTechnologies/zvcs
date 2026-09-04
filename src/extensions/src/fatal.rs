//! Errors that are git's, told apart from errors that are this port's.
//!
//! git's `die()` writes `fatal: <message>` to stderr and exits 128. Nearly every
//! diagnostic a caller can provoke — a bad revision, a contradictory pair of
//! options, an unmerged path — arrives in that shape, and callers read it: a
//! client checks the exit code, and some parse the text.
//!
//! This port also has to say things git never says: that a feature is not ported.
//! That is a different claim, and it must not wear git's clothes. `fatal: …` with
//! exit 128 asserts "this is what git does here"; a port that has not implemented
//! something and says so in git's voice is lying about its own coverage, which is
//! worse than the gap it is papering over.
//!
//! So the two are distinct types. [`Fatal`] is the first — a message git itself
//! would `die()` with, rendered exactly as git renders it. An ordinary
//! `anyhow::bail!` is the second, and keeps the `zvcs: <verb>: …` prefix and exit
//! 1 that mark it as this binary speaking for itself. `run_command` in `lib.rs`
//! is the one place that tells them apart.

use std::fmt;

/// A message git would `die()` with: `fatal: <message>` on stderr, exit 128.
#[derive(Debug)]
pub struct Fatal(pub String);

impl fmt::Display for Fatal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Without the prefix: the renderer adds it, and a `Fatal` that ends up
        // inside someone else's context chain should read as the bare message.
        f.write_str(&self.0)
    }
}

impl std::error::Error for Fatal {}

/// git's exit code for `die()`.
pub const EXIT_FATAL: u8 = 128;

/// A diagnostic already written to stderr; only the exit code is left to carry.
///
/// Not every git diagnostic is a `die()`. `report_path_error()` writes `error: …`
/// and lets the caller return 1; `parse_options` writes `error: …` followed by the
/// usage block and exits 129. Those have already said their piece by the time they
/// unwind, so the renderer must add nothing — printing `zvcs: <verb>: …` after
/// them would double the message and change the exit code.
#[derive(Debug)]
pub struct Silent(pub u8);

impl fmt::Display for Silent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("")
    }
}

impl std::error::Error for Silent {}

/// Return early with a message git would `die()` with — `bail!` for git's voice.
///
/// ```ignore
/// git_fatal!("No paths with --include/--only does not make sense.");
/// git_fatal!("could not lookup commit {rev}");
/// ```
#[macro_export]
macro_rules! git_fatal {
    ($($arg:tt)*) => {
        return ::std::result::Result::Err(
            ::anyhow::Error::new($crate::fatal::Fatal(::std::format!($($arg)*)))
        )
    };
}

/// The same message as [`git_fatal!`] as a value, for the places that build an
/// error instead of returning one — `ok_or_else`, `map_err`, and friends.
pub fn die(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Fatal(message.into()))
}

/// git's `setup_work_tree()`: the commands that need a work tree die with this
/// when setup did not find one, which is what standing in a `.git` directory or
/// a bare repository leaves them with.
pub fn need_work_tree() -> anyhow::Error {
    die("this operation must be run in a work tree")
}

// ---------------------------------------------------------------------------
// setup.c: "setup did not find a repository"
// ---------------------------------------------------------------------------

/// `DEFAULT_GIT_DIR_ENVIRONMENT` in `environment.h`. git fills the discovery
/// message with this constant, not with a path: when the upward search comes up
/// empty the message names the thing it was looking *for*, and is the same in
/// every directory.
const DEFAULT_GIT_DIR: &str = ".git";

/// `GIT_DIR_HIT_CEILING` in `setup_git_directory_gently()`: the upward search
/// reached the top — of the filesystem, or of a `GIT_CEILING_DIRECTORIES` entry
/// — without finding a repository. Both endings print this same line; git does
/// not say which one stopped it, nor how far it walked.
pub fn no_repository_walked() -> String {
    format!("not a git repository (or any of the parent directories): {DEFAULT_GIT_DIR}")
}

/// `GIT_DIR_HIT_MOUNT_POINT`: the search would have had to leave the filesystem
/// it started on, which it will not do unless `GIT_DISCOVERY_ACROSS_FILESYSTEM`
/// says so. `dir` is the directory the crossing was detected at.
pub fn no_repository_at_mount_point(dir: &std::path::Path) -> String {
    format!(
        "not a git repository (or any parent up to mount point {})\n\
         Stopping at filesystem boundary (GIT_DISCOVERY_ACROSS_FILESYSTEM not set).",
        dir.display()
    )
}

/// `setup_explicit_git_dir()`: `$GIT_DIR` was set, so nothing was searched for
/// — the named directory simply is not a repository, and git quotes it back.
pub fn no_repository_explicit(git_dir: &str) -> String {
    format!("not a git repository: '{git_dir}'")
}

/// The line `setup_git_directory_gently()` would die with *here*, for the call
/// sites that know only that they found no repository and never held the error.
///
/// Only the two endings reachable without one are distinguished: an explicit
/// `$GIT_DIR` skips discovery entirely, and everything else walked and failed.
/// A filesystem boundary is invisible from here — it is a property of the walk
/// — so a site that has the error should map it with [`discovery_message`].
pub fn no_repository() -> String {
    match std::env::var_os("GIT_DIR") {
        Some(dir) => no_repository_explicit(&dir.to_string_lossy()),
        None => no_repository_walked(),
    }
}

/// The same, as an error to return: `fatal: <message>`, exit 128.
pub fn no_repository_error() -> anyhow::Error {
    die(no_repository())
}

/// How git's setup would have reported a `gix::discover()` failure, or `None`
/// when the failure is not one setup has a diagnostic for — an unreadable
/// directory, a dubious-ownership refusal, a config that would not parse. Those
/// keep the port's own voice rather than borrowing a message git never prints
/// for them.
pub fn discovery_message(err: &gix::discover::Error) -> Option<String> {
    match err {
        gix::discover::Error::Discover(err) => upwards_message(err),
        // `discover()` reaches `open()` two ways: `$GIT_DIR` short-circuits the
        // search (git's `GIT_DIR_EXPLICIT`), and a successful walk still has to
        // open what it found. Only the first is `setup_explicit_git_dir()`, and
        // only it names a path in the message — so the variable has to be set
        // for this to be that branch at all.
        gix::discover::Error::Open(gix::open::Error::NotARepository { .. }) => {
            std::env::var_os("GIT_DIR").map(|dir| no_repository_explicit(&dir.to_string_lossy()))
        }
        gix::discover::Error::Open(_) => None,
    }
}

/// The discovery-walk half of [`discovery_message`].
fn upwards_message(err: &gix::discover::upwards::Error) -> Option<String> {
    use gix::discover::upwards::Error as E;
    match err {
        // git collapses "ran out of parents" and "hit a ceiling" into one
        // ending, `GIT_DIR_HIT_CEILING`, and one message.
        E::NoGitRepository { .. } | E::NoGitRepositoryWithinCeiling { .. } => {
            Some(no_repository_walked())
        }
        E::NoGitRepositoryWithinFs { limit, .. } => Some(no_repository_at_mount_point(limit)),
        _ => None,
    }
}

/// [`discovery_message`] for an error that has already been wrapped: the whole
/// `anyhow` chain is searched, since a discovery failure usually arrives inside
/// whatever context the command added on the way out.
pub fn discovery_fatal(err: &anyhow::Error) -> Option<String> {
    err.chain().find_map(|cause| {
        if let Some(err) = cause.downcast_ref::<gix::discover::Error>() {
            return discovery_message(err);
        }
        cause.downcast_ref::<gix::discover::upwards::Error>().and_then(upwards_message)
    })
}

// ---------------------------------------------------------------------------
// read-cache.c and refs.c: data git cannot read is a `die()`, not a return 1
// ---------------------------------------------------------------------------

/// Whether the failure is "the on-disk repository data could not be read at the
/// width the repository declares", which git ends at exit 128 rather than 1.
///
/// Two front doors reach the same ending in git:
///
/// * **The index.** Every path into `do_read_index()` that cannot produce a
///   state calls `die()`: `"%s: index file open failed"`, `"index file smaller
///   than expected"`, `"bad index file sha1 signature"`, `"index file corrupt"`,
///   and — the one this port meets most often — `"unknown index entry format
///   0x%08x"` from `create_from_disk()`. There is no arm that returns a
///   non-fatal failure, so an unreadable index is 128 without exception.
/// * **A loose ref whose contents will not parse.** `refs.c` reports it as a
///   *broken ref* rather than as an error on the store, and what happens next is
///   the verb's decision: `for-each-ref` warns (`warning: ignoring broken ref
///   <name>`) and exits 0, while `log`, `show-ref`, `branch` and
///   `symbolic-ref` die at 128 with their own sentences. The verbs that
///   tolerate it handle it themselves before the error ever reaches here; what
///   arrives here is the half git dies on.
///
/// Measured on git 2.55.0 against a repository declaring
/// `extensions.objectFormat = sha256` over a SHA-1 store, and again against a
/// SHA-1 repository with a hand-corrupted `.git/index` and `.git/refs/heads/main`
/// — the exit codes are the same in both, so this is the general condition and
/// not a property of the mislabelled-hash fixture.
///
/// **Only the exit code is borrowed, never the sentence.** git's index message
/// carries four bytes of stat data read back as a format word
/// (`0xb17b0000` on one run of this fixture, a different value on the next) and
/// the port's carries a rolling hash, so neither side has a reproducible string
/// and this port must not invent one in git's voice. The caller therefore keeps
/// the port's own `zvcs: <verb>: …` diagnostic and changes only the status,
/// which is the part a caller branches on.
pub fn unreadable_repository_data(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        // The index. `gix::worktree::open_index::Error` is the wrapper every
        // call site in this port returns, and it is the *outermost* type in the
        // chain — every layer beneath it is `#[error(transparent)]`, and
        // `transparent` forwards `source()` as well as `Display`, so
        // `gix_index::file::init::Error` and `gix_index::decode::Error` are
        // skipped entirely and downcasting to them finds nothing. The two
        // narrower types are still tried for the call sites that open an index
        // file directly rather than through the repository.
        if let Some(err) = cause.downcast_ref::<gix::worktree::open_index::Error>() {
            // Only the two arms that are about the *file*. The config arms are
            // a bad `index.threads`/`index.skipHash` value, which is a
            // configuration failure and belongs to `config::setup_fatal`.
            return matches!(
                err,
                gix::worktree::open_index::Error::IndexFile(_)
                    | gix::worktree::open_index::Error::IndexCorrupt(_)
            );
        }
        if cause.downcast_ref::<gix::index::file::init::Error>().is_some()
            || cause.downcast_ref::<gix::index::decode::Error>().is_some()
        {
            return true;
        }
        // A ref file whose body is neither an object id of this repository's
        // width nor a valid `ref:` line. This is the *leaf* of the chain for the
        // same transparency reason as above — `gix_ref::file::find::Error` and
        // both of its `gix` wrappers forward straight past themselves — so it is
        // the one link that is always reachable whichever front door the verb
        // used. `RefnameValidation` is excluded: a symbolic ref pointing at an
        // invalid name is a different condition and git does not die alike on it.
        if let Some(err) = cause.downcast_ref::<gix::refs::file::loose::reference::decode::Error>() {
            return matches!(err, gix::refs::file::loose::reference::decode::Error::Parse { .. });
        }
        false
    })
}
