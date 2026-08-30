//! The `$ZVCS_HOME` registries (`zguard`, `zintercept`, `zsched`): ids that are
//! never handed out twice, and a lock that keeps two writers from losing each
//! other's entries.
//!
//! Each registry used to number a new entry `max(existing) + 1`. That reuses an
//! id as soon as the entry holding the highest one is removed: with rules #1 and
//! #2, `zguard rm 2` then `zguard deny 'ccc*'` produces a *new* rule #2. Ids are
//! how these verbs are driven — `zguard rm <id>`, `zintercept remove <id>`,
//! `zsched rm <id>` — so a script or a person holding "#2" now points at
//! something else, and removes or trusts the wrong entry.
//!
//! The high-water mark lives beside the registry as `<name>.next`, so an id is
//! retired with its entry. A registry written before this file existed simply
//! has no mark: numbering continues from its highest entry and is monotonic from
//! then on, which is exactly what those installations already believed.

use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// An exclusive hold on one registry, for the whole of a read-modify-write.
///
/// These registries are rewritten whole: a verb loads every entry, changes one,
/// and writes the file back. Two of those in flight at once lose an entry — with
/// twelve concurrent `git zguard deny`, eleven rules survived and all twelve
/// commands reported success. A dropped `deny` rule is a policy the user
/// believes is in force, and this project expects sixteen agents working at
/// once, so the race is the normal case rather than the exotic one.
///
/// `flock` is released by the kernel when the holder exits, so a crash cannot
/// wedge a registry. A filesystem without `flock` yields `None`: no exclusion is
/// available there and the write proceeds as it always did, rather than failing.
pub(crate) struct RegistryLock(std::fs::File);

impl Drop for RegistryLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned by this value and still open.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Take the lock for registry `name`, blocking until it is ours.
pub(crate) fn lock(name: &str) -> Option<RegistryLock> {
    lock_in(&crate::superset::zdaemon::zvcs_home(), name)
}

/// [`lock`] against an explicit home, so tests need not touch `ZVCS_HOME`.
pub(crate) fn lock_in(home: &Path, name: &str) -> Option<RegistryLock> {
    let _ = std::fs::create_dir_all(home);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join(format!("{name}.lock")))
        .ok()?;
    // SAFETY: `file` owns the descriptor for the whole call and `flock` only
    // reads it. Blocking is what we want: these holds are one small file write,
    // and a queue of writers is exactly what must not turn into lost entries.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return None;
    }
    Some(RegistryLock(file))
}

fn mark_path(home: &Path, name: &str) -> PathBuf {
    home.join(format!("{name}.next"))
}

/// The id to give the next entry of registry `name`, given the highest id its
/// entries currently carry (0 when empty).
///
/// The mark is advanced before the caller writes its file. A mark that cannot be
/// written leaves the id correct for this call and the registry's own write is
/// what reports the unwritable home, so nothing is silently numbered twice
/// without something else failing first.
pub(crate) fn next_id(name: &str, highest_in_use: u64) -> u64 {
    next_id_in(&crate::superset::zdaemon::zvcs_home(), name, highest_in_use)
}

/// [`next_id`] against an explicit home — the whole of the logic, so it can be
/// tested without touching the process-wide `ZVCS_HOME` that every other test in
/// this binary is reading at the same time.
pub(crate) fn next_id_in(home: &Path, name: &str, highest_in_use: u64) -> u64 {
    let path = mark_path(home, name);
    let stored = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let id = stored.max(highest_in_use + 1);
    let _ = std::fs::create_dir_all(home);
    let _ = std::fs::write(&path, (id + 1).to_string());
    id
}

#[cfg(test)]
mod tests {
    use super::next_id_in;
    use std::path::PathBuf;

    /// A scratch home for one test. Nothing process-wide is touched, so these
    /// run in parallel with each other and with everything else.
    fn home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zvcs-regid-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_id_is_never_handed_out_twice() {
        let h = home("norepeat");
        // Two entries handed out, then the higher one removed: the registry's
        // highest in-use id falls back to 1, and the next id must still be 3.
        assert_eq!(next_id_in(&h, "t", 0), 1);
        assert_eq!(next_id_in(&h, "t", 1), 2);
        assert_eq!(
            next_id_in(&h, "t", 1),
            3,
            "the id freed by a removal must not come back"
        );
        assert_eq!(
            next_id_in(&h, "t", 0),
            4,
            "an emptied registry must not restart from 1"
        );
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn a_registry_written_before_the_mark_existed_continues_from_its_entries() {
        let h = home("legacy");
        // No mark on disk and entries up to #7: numbering picks up at 8 rather
        // than colliding with what is already there.
        assert_eq!(next_id_in(&h, "legacy", 7), 8);
        assert_eq!(next_id_in(&h, "legacy", 8), 9);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn registries_do_not_share_a_counter() {
        let h = home("separate");
        assert_eq!(next_id_in(&h, "a", 0), 1);
        assert_eq!(
            next_id_in(&h, "b", 0),
            1,
            "each registry numbers its own entries"
        );
        assert_eq!(next_id_in(&h, "a", 1), 2);
        let _ = std::fs::remove_dir_all(&h);
    }
}
