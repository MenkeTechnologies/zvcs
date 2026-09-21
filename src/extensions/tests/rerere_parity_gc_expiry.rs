//! `git rerere gc` reads its two expiry knobs the way
//! `repo_config_get_expiry_in_days()` (config.c:2482) does, and skips every
//! `rr-cache` entry whose basename is not a whole object name
//! (`is_rr_cache_dirname()`, rerere.c:1222).
//!
//! Both were measured against stock git 2.55.0:
//!
//! * `git -c gc.rerereresolved=now -c gc.rerereunresolved=now rerere gc` empties
//!   a cache holding two-day-old records. A port that only understands an
//!   integer number of days leaves the default 60/15-day cutoffs in place and
//!   keeps them, while still agreeing on `5.days.ago` — which is why the integer
//!   half alone looks correct until `now` is asked for.
//! * `rerere gc` on a cache that also holds `notanoid/`, `emptydir/` and
//!   `<oid>xx/` leaves all three alone, entries and directories both. A port
//!   that scans every directory entry prunes their files and removes the
//!   now-empty directories.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new(BIN).args(args).current_dir(dir).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with rerere enabled and an empty `rr-cache`.
fn fixture(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-rrgc-parity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "author@example.com"]);
    git(&repo, &["config", "user.name", "A U Thor"]);
    git(&repo, &["config", "rerere.enabled", "true"]);
    std::fs::write(repo.join("f"), "one\n").unwrap();
    git(&repo, &["add", "f"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=author@example.com",
            "commit",
            "-q",
            "-m",
            "base",
            "--date=2005-04-07T15:13:13-07:00",
        ],
    );
    std::fs::create_dir_all(repo.join(".git/rr-cache")).unwrap();
    repo
}

/// Backdate `path` by `days`, as `test-tool chmtime` does in git's own tests.
fn backdate(path: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_secs(days * 86400);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_accessed(when).set_modified(when))
        .unwrap();
}

/// One `rr-cache/<id>` holding a resolved record (preimage + postimage), both
/// two days old.
fn resolved_entry(repo: &Path, id: &str) -> PathBuf {
    let dir = repo.join(".git/rr-cache").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["preimage", "postimage"] {
        let f = dir.join(name);
        std::fs::write(&f, "x\n").unwrap();
        backdate(&f, 2);
    }
    dir
}

fn files_under(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

#[test]
fn gc_expiry_accepts_the_approxidate_spellings() {
    let repo = fixture("expiry");
    let id = "0123456789abcdef0123456789abcdef01234567";
    let entry = resolved_entry(&repo, id);

    // Five days back: the two-day-old record is younger than the cutoff either
    // way, so this run must keep it. It is the control for the run below — a
    // port that ignores the string entirely also passes this one.
    git(
        &repo,
        &[
            "-c",
            "gc.rerereresolved=5.days.ago",
            "-c",
            "gc.rerereunresolved=5.days.ago",
            "rerere",
            "gc",
        ],
    );
    assert!(
        entry.join("postimage").exists() && entry.join("preimage").exists(),
        "gc.rerereresolved=5.days.ago expired a two-day-old resolution"
    );

    // `now` is `parse_expiry_date()`'s TIME_MAX (date.c:963-972): everything in
    // the past goes.
    git(
        &repo,
        &["-c", "gc.rerereresolved=now", "-c", "gc.rerereunresolved=now", "rerere", "gc"],
    );
    assert_eq!(
        files_under(&repo.join(".git/rr-cache")),
        Vec::<String>::new(),
        "gc.rerereresolved=now left records behind, so the value never reached parse_expiry_date()"
    );
    assert!(
        !entry.exists(),
        "the emptied rr-cache/<id> directory should have been removed with its records"
    );
}

#[test]
fn gc_expiry_never_keeps_everything() {
    let repo = fixture("never");
    let id = "89abcdef0123456789abcdef0123456789abcdef";
    let entry = resolved_entry(&repo, id);
    // A really old record, so only an expiry of 0 can save it.
    backdate(&entry.join("preimage"), 400);
    backdate(&entry.join("postimage"), 400);

    git(
        &repo,
        &[
            "-c",
            "gc.rerereresolved=never",
            "-c",
            "gc.rerereunresolved=never",
            "rerere",
            "gc",
        ],
    );
    let mut want = vec![format!("{id}/preimage"), format!("{id}/postimage")];
    want.sort();
    assert_eq!(
        files_under(&repo.join(".git/rr-cache")),
        want,
        "gc.rerere*=never must expire nothing at all"
    );
}

#[test]
fn gc_leaves_entries_that_are_not_object_names_alone() {
    let repo = fixture("dirname");
    let rr = repo.join(".git/rr-cache");
    // A real entry, expired, to prove the collector did run.
    let real = resolved_entry(&repo, "fedcba9876543210fedcba9876543210fedcba98");
    backdate(&real.join("preimage"), 400);
    backdate(&real.join("postimage"), 400);

    // Three basenames `parse_oid_hex()` cannot consume whole.
    for name in ["notanoid", "emptydir", "0123456789abcdef0123456789abcdef01234567xx"] {
        std::fs::create_dir_all(rr.join(name)).unwrap();
    }
    let stray = rr.join("notanoid/preimage");
    std::fs::write(&stray, "not ours\n").unwrap();
    backdate(&stray, 400);

    git(&repo, &["rerere", "gc"]);

    assert!(
        !real.exists(),
        "the expired record was kept, so this run proves nothing about the skip"
    );
    assert!(
        stray.exists(),
        "rerere gc pruned a file under a directory whose name is not an object name"
    );
    for name in ["notanoid", "emptydir", "0123456789abcdef0123456789abcdef01234567xx"] {
        assert!(
            rr.join(name).is_dir(),
            "rerere gc removed rr-cache/{name}, which is not an rr-cache entry"
        );
    }
}
