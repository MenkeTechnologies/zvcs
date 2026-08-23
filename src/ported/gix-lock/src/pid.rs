//! The `core.lockfilePid` companion file — a `<resource>~pid.lock` written
//! alongside a `<resource>.lock` this crate takes, and the diagnosis it buys
//! when somebody *else* already holds that lock.
//!
//! Port of git 2.55.0's `lockfile.c:80-165` (the PID-file block) and
//! `lockfile.c:254-300` (`unable_to_lock_message()`), against the declarations
//! in `lockfile.h:120-146`. Git's own comment states the naming rule and why the
//! infix cannot collide with a refname (`lockfile.c:80-88`):
//!
//! ```c
//! /*
//!  * Lock PID file functions - write PID to a foo~pid.lock file alongside
//!  * the lock file for debugging stale locks. The PID file is registered
//!  * as a tempfile so it gets cleaned up by signal/atexit handlers.
//!  *
//!  * Naming: For "foo.lock", the PID file is "foo~pid.lock". The tilde is
//!  * forbidden in refnames and allowed in Windows filenames, guaranteeing
//!  * no collision with the refs namespace.
//!  */
//! ```
//!
//! The switch really is a process-global in git — `lockfile.c:90-91` declares it
//! and `environment.c:532-535` assigns it straight out of
//! `git_default_core_config()`:
//!
//! ```c
//! if (!strcmp(var, "core.lockfilepid")) {
//!         lockfile_pid_enabled = git_config_bool(var, value);
//!         return 0;
//! }
//! ```
//!
//! so it is one here too, and the configuration layer above sets it at the same
//! point in the same callback. Default is off, matching a C global's zero
//! initialisation.
//!
//! ### The asymmetry that makes the feature useful
//!
//! Writing the companion is gated on the switch; *reading* it is not.
//! `unable_to_lock_message()` tries the companion path unconditionally
//! (`lockfile.c:269-273`, comment and all), because the process that left the
//! lock behind may have had the key on even though the process now reporting the
//! failure has it off. So [`holder`] and [`unable_to_lock_message`] never consult
//! [`enabled`].
//!
//! ### Where the path comes from
//!
//! Git derives both the lock path and the companion path from `base_path`, which
//! is the resource path after `resolve_symlink()` unless `LOCK_NO_DEREF`
//! (`lockfile.c:175-180`). This crate does not resolve symlinks for the lock path
//! either — `acquire::add_lock_suffix` appends to the resource as given — so
//! deriving the companion from the same unresolved resource keeps the two
//! consistent with each other, which is the property that matters.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gix_tempfile::{AutoRemove, ContainingDirectory};

/// `LOCK_PID_INFIX` (`lockfile.h:136`) — inserted between the resource path and
/// the `.lock` suffix to name the PID file.
pub const INFIX: &str = "~pid";

/// `LOCK_PID_MAXLEN` (`lockfile.h:140`).
///
/// Despite the name it is **not** a cap. It is the third argument git hands
/// `strbuf_read_file()` (`lockfile.c:147`), which `strbuf.h:466-471` documents as
/// a size *hint* used "to avoid reallocs" — the call still reads the whole file.
/// [`read_lock_pid`] therefore reads the file entirely, as git does, and this
/// constant exists only to name the value git passes.
pub const MAXLEN: usize = 32;

/// `int lockfile_pid_enabled` (`lockfile.c:91`). Zero-initialised, i.e. off,
/// until the `core.lockfilePid` arm of the default-config callback assigns it.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Assign git's `lockfile_pid_enabled` global — what `environment.c:533` does
/// with the boolean value of `core.lockfilePid`.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Read git's `lockfile_pid_enabled` global.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// `get_pid_path()` (`lockfile.c:105-110`): the resource path with `~pid.lock`
/// appended. A plain concatenation, exactly as in C — `foo.ext` yields
/// `foo.ext~pid.lock` next to the lock at `foo.ext.lock`.
pub fn path_for(resource: &Path) -> PathBuf {
    let mut buf = resource.as_os_str().to_os_string();
    buf.push(INFIX);
    buf.push(crate::DOT_LOCK_SUFFIX);
    buf.into()
}

/// `strerror(err)` as git prints it, without Rust's ` (os error N)` tail.
///
/// git interpolates `strerror(err)` directly into both branches of
/// `unable_to_lock_message()` (`lockfile.c:267`, `:297`), so `File exists` has to
/// arrive bare — `std::io::Error`'s own `Display` renders
/// `File exists (os error 17)` and would no longer match.
///
/// The text before that tail *is* `strerror`: std formats an OS error as
/// `"{detail} (os error {code})"` where `detail` comes from `strerror_r`. So
/// trimming the tail it appended, and only when there is a code to have appended
/// it, recovers exactly the bytes git prints.
fn strerror(err: &std::io::Error) -> String {
    let rendered = err.to_string();
    match err.raw_os_error() {
        Some(code) => rendered
            .strip_suffix(&format!(" (os error {code})"))
            .unwrap_or(rendered.as_str())
            .to_owned(),
        None => rendered,
    }
}

/// `create_lock_pid_file()` (`lockfile.c:112-139`).
///
/// Writes `pid <getpid()>\n` into an `O_EXCL`-created file next to the lock and
/// registers it as a tempfile so a signal or `atexit` removes it. Every failure
/// path in the C is silent (`goto out` with a NULL result) except a short write,
/// which warns and unlinks; a `None` here is that same "no PID file, carry on".
///
/// The `O_EXCL` matters and is preserved: `gix_tempfile`'s `at_path`
/// (`gix-tempfile/src/handle.rs:26-54`) builds the file through
/// `tempfile::Builder::rand_bytes(0).tempfile_in(..)`, which creates with
/// `O_CREAT | O_EXCL`. A companion left behind by a crashed holder therefore
/// makes this return `None` rather than being overwritten — `lockfile.c:121-123`.
///
/// The lock itself has already been taken when this runs, so the containing
/// directory is known to exist and only the tempfile itself needs removing —
/// git registers the PID file with plain `register_tempfile()`, which likewise
/// prunes no directories.
pub(crate) fn create(
    resource: &Path,
    permissions: Option<std::fs::Permissions>,
) -> Option<gix_tempfile::Handle<gix_tempfile::handle::Closed>> {
    if !enabled() {
        return None;
    }
    let pid_path = path_for(resource);
    let handle = match permissions {
        Some(permissions) => gix_tempfile::writable_at_with_permissions(
            &pid_path,
            ContainingDirectory::Exists,
            AutoRemove::Tempfile,
            permissions,
        ),
        None => gix_tempfile::writable_at(&pid_path, ContainingDirectory::Exists, AutoRemove::Tempfile),
    };
    let mut handle = handle.ok()?;
    let content = format!("pid {}\n", std::process::id());
    let written = handle
        .with_mut(|tf| std::io::Write::write_all(tf.as_file_mut(), content.as_bytes()))
        .and_then(|res| res);
    if let Err(err) = written {
        // `warning_errno(_("could not write lock pid file '%s'"), pid_path)`
        // (`lockfile.c:127`) — `warning_errno` appends `: <strerror>` to the
        // format — then `unlink()`; dropping the handle is that unlink.
        eprintln!(
            "warning: could not write lock pid file '{}': {}",
            pid_path.display(),
            strerror(&err)
        );
        return None;
    }
    handle.close().ok()
}

/// `read_lock_pid()` (`lockfile.c:141-165`), as the `Option` its `int` return
/// really is.
///
/// A missing or empty companion is `None` and says nothing: git's
/// `strbuf_read_file(...) <= 0` jumps straight to `out`, past the warning
/// (`lockfile.c:147-148`). A companion that exists but does not parse is `None`
/// *and* warns, because `ret` is still `-1` when control reaches
/// `lockfile.c:159-160`.
///
/// The grammar is `skip_prefix(content, "pid ")` over the right-trimmed contents,
/// then `strtoumax` base 10 with `*pid_out > 0 && !*endptr` — so the digits must
/// run to the very end and zero is not a PID.
///
/// `strtoumax` is more permissive than the `pid <digits>\n` that [`create`]
/// writes, and [`parse_umax`] reproduces it rather than tightening it. Measured
/// against git 2.55.0, with a companion beside a held `refs/heads/main.lock`:
///
/// ```text
/// pid 12    -> Lock was held by process 12
/// pid  12   -> Lock was held by process 12                    (leading space skipped)
/// pid +12   -> Lock was held by process 12                    (leading sign)
/// pid -12   -> Lock was held by process 18446744073709551604  (negation wraps)
/// pid 12    -> accepted with no trailing newline too
/// pid 0     -> warning: malformed lock pid file '…'
/// pid 12x   -> warning: malformed lock pid file '…'
/// pid12     -> warning: malformed lock pid file '…'
/// PID 12    -> warning: malformed lock pid file '…'
/// ```
pub fn read_lock_pid(pid_path: &Path) -> Option<u64> {
    // `strbuf_read_file`'s third argument is a sizing hint, not a limit
    // (`strbuf.h:466-471`), so the whole file is read exactly as in C.
    let content = std::fs::read(pid_path).ok()?;
    if content.is_empty() {
        return None;
    }

    // `strbuf_rtrim()` — `while (len && isspace(buf[len - 1])) len--`.
    let trimmed = {
        let mut end = content.len();
        while end > 0 && is_c_space(content[end - 1]) {
            end -= 1;
        }
        &content[..end]
    };

    match trimmed.strip_prefix(b"pid ").and_then(parse_umax) {
        // `*pid_out > 0` — a zero PID leaves `ret` at -1 and warns.
        Some(pid) if pid > 0 => Some(pid),
        _ => {
            eprintln!("warning: malformed lock pid file '{}'", pid_path.display());
            None
        }
    }
}

/// C's `isspace()` for the "C" locale, which `strbuf_rtrim` and `strtoumax` both
/// use. Rust's `u8::is_ascii_whitespace` omits the vertical tab, so it is spelled
/// out here.
fn is_c_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `strtoumax(val, &endptr, 10)` under git's `!*endptr` test — the whole
/// remainder has to be consumed, so this returns `None` for anything with a tail.
///
/// Optional leading whitespace and an optional `+`/`-` are part of the C grammar.
/// A `-` negates in unsigned arithmetic, which wraps; overflow saturates at
/// `UINTMAX_MAX` and git does not consult `errno`, so it accepts that too.
fn parse_umax(text: &[u8]) -> Option<u64> {
    let mut rest = text;
    while rest.first().copied().is_some_and(is_c_space) {
        rest = &rest[1..];
    }
    let negate = match rest.first() {
        Some(b'+') => {
            rest = &rest[1..];
            false
        }
        Some(b'-') => {
            rest = &rest[1..];
            true
        }
        _ => false,
    };
    if rest.is_empty() || !rest.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let magnitude = rest
        .iter()
        .try_fold(0u64, |acc, digit| {
            acc.checked_mul(10)?.checked_add(u64::from(digit - b'0'))
        })
        .unwrap_or(u64::MAX);
    Some(if negate { magnitude.wrapping_neg() } else { magnitude })
}

/// What the companion beside a contended lock says about its holder — git's
/// `pid_status` local (`lockfile.c:261`), which is `0` unknown, `1` running,
/// `-1` stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    /// `pid_status == 0`: no companion, an unreadable one, or a `kill()` that
    /// failed for a reason other than `ESRCH`.
    Unknown,
    /// `pid_status == 1`: `kill(pid, 0)` succeeded, or failed with `EPERM` —
    /// which still proves the process exists, this user just may not signal it.
    Running(u64),
    /// `pid_status == -1`: `kill(pid, 0)` failed with `ESRCH`, so the holder is
    /// gone and the lock it left is stale.
    Stale(u64),
}

/// `unable_to_lock_message()`'s probe of the holder (`lockfile.c:264-278`).
///
/// Deliberately not gated on [`enabled`]: the companion is read unconditionally
/// because it "may exist if core.lockfilePid was enabled" for whoever took the
/// lock, which is not necessarily this process (`lockfile.c:269-272`).
pub fn holder(resource: &Path) -> Holder {
    let Some(pid) = read_lock_pid(&path_for(resource)) else {
        return Holder::Unknown;
    };
    signal_probe(pid)
}

/// `kill((pid_t)pid, 0)` and the two `errno` tests around it
/// (`lockfile.c:274-277`).
///
/// `rustix::process::test_kill_process` *is* `kill(pid, 0)` — the signal-less
/// existence-and-permission probe — reached without an `unsafe` block, which this
/// crate forbids.
///
/// A PID too large for the platform's `pid_t` cannot name a live process, so
/// `Pid::from_raw` rejecting it is the same answer `kill` would give: nothing to
/// find, therefore a stale lock. git reaches that by truncating to `pid_t` and
/// letting `kill` fail `ESRCH`.
#[cfg(not(windows))]
fn signal_probe(pid: u64) -> Holder {
    let Ok(raw) = i32::try_from(pid) else {
        return Holder::Stale(pid);
    };
    let Some(target) = rustix::process::Pid::from_raw(raw) else {
        return Holder::Stale(pid);
    };
    match rustix::process::test_kill_process(target) {
        Ok(()) => Holder::Running(pid),
        // `errno == EPERM` — the process is there, we simply may not signal it.
        Err(rustix::io::Errno::PERM) => Holder::Running(pid),
        Err(rustix::io::Errno::SRCH) => Holder::Stale(pid),
        Err(_) => Holder::Unknown,
    }
}

/// Windows has no `kill(2)`; git reaches this through its `compat/mingw.c` shim,
/// which this crate does not carry. Reporting [`Holder::Unknown`] falls back to
/// the same sentence git prints when the companion is absent, which is the
/// truthful answer when the holder cannot be probed at all.
#[cfg(windows)]
fn signal_probe(_pid: u64) -> Holder {
    Holder::Unknown
}

/// The holder clause of `unable_to_lock_message()` (`lockfile.c:280-292`) — the
/// second paragraph, verbatim in all three of its spellings.
pub fn holder_clause(resource: &Path) -> String {
    match holder(resource) {
        Holder::Running(pid) => format!(
            "Lock may be held by process {pid}; if no git process is running, \
             the lock file may be stale (PIDs can be reused)"
        ),
        Holder::Stale(pid) => format!(
            "Lock was held by process {pid}, which is no longer running; \
             the lock file appears to be stale"
        ),
        Holder::Unknown => {
            "Another git process seems to be running in this repository, \
             or the lock file may be stale"
                .into()
        }
    }
}

/// `unable_to_lock_message()` (`lockfile.c:254-300`) in full, for `resource` and
/// the `errno` that stopped the lock being taken.
///
/// `EEXIST` is the contended case and gets the two-paragraph message: the header
/// naming the absolute *lock* path, a blank line, then [`holder_clause`]. Any
/// other error gets the one-line form, which names the resource and appends
/// `.lock` in the format string rather than using the lock path
/// (`lockfile.c:296-299`).
pub fn unable_to_lock_message(resource: &Path, err: &std::io::Error) -> String {
    // `absolute_path(path)` (`lockfile.c:257`, `:298`).
    let absolute = std::path::absolute(resource).unwrap_or_else(|_| resource.to_owned());
    if err.kind() == std::io::ErrorKind::AlreadyExists {
        format!(
            "Unable to create '{}{}': {}.\n\n{}",
            absolute.display(),
            crate::DOT_LOCK_SUFFIX,
            strerror(err),
            holder_clause(resource)
        )
    } else {
        format!(
            "Unable to create '{}{}': {}",
            absolute.display(),
            crate::DOT_LOCK_SUFFIX,
            strerror(err)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three spellings of the holder clause, keyed off what the companion
    /// says — `lockfile.c:280-292`.
    #[test]
    fn holder_clause_matches_gits_three_sentences() {
        let dir = tempfile::tempdir().unwrap();
        let resource = dir.path().join("index");

        // No companion at all: `pid_status` stays 0.
        assert_eq!(
            holder_clause(&resource),
            "Another git process seems to be running in this repository, or the lock file may be stale"
        );

        // A PID that cannot exist: `kill` returns `ESRCH`.
        std::fs::write(path_for(&resource), "pid 4194303\n").unwrap();
        assert_eq!(
            holder_clause(&resource),
            "Lock was held by process 4194303, which is no longer running; the lock file appears to be stale"
        );

        // This very process: `kill(self, 0)` succeeds.
        let me = std::process::id();
        std::fs::write(path_for(&resource), format!("pid {me}\n")).unwrap();
        assert_eq!(
            holder_clause(&resource),
            format!(
                "Lock may be held by process {me}; if no git process is running, \
                 the lock file may be stale (PIDs can be reused)"
            )
        );
    }

    /// `read_lock_pid()`'s grammar, including the two inputs that must *not*
    /// warn (`lockfile.c:147-148`) and the ones that must.
    #[test]
    fn read_lock_pid_accepts_only_gits_own_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().join("p");

        // Absent and empty are silent `None`s, not malformed ones.
        assert_eq!(read_lock_pid(&at), None);
        std::fs::write(&at, "").unwrap();
        assert_eq!(read_lock_pid(&at), None);

        // The spelling `create()` writes, with the trailing newline rtrimmed.
        std::fs::write(&at, "pid 1234\n").unwrap();
        assert_eq!(read_lock_pid(&at), Some(1234));

        // `*pid_out > 0` rejects zero; `!*endptr` rejects a trailing tail; the
        // prefix must be exactly `pid `.
        for malformed in ["pid 0\n", "pid 12x\n", "pid\n", "1234\n", "PID 1234\n"] {
            std::fs::write(&at, malformed).unwrap();
            assert_eq!(read_lock_pid(&at), None, "should reject {malformed:?}");
        }
    }

    /// The non-`EEXIST` branch names the resource with `.lock` glued on by the
    /// format string and carries no holder paragraph (`lockfile.c:296-299`).
    #[test]
    fn a_non_contention_error_gets_the_one_line_form() {
        let dir = tempfile::tempdir().unwrap();
        let resource = dir.path().join("index");
        let err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let msg = unable_to_lock_message(&resource, &err);
        assert!(msg.starts_with(&format!("Unable to create '{}.lock': ", resource.display())));
        assert!(!msg.contains("\n\n"), "one-line form has no holder paragraph: {msg}");
    }
}
