//! When `maintenance run --auto` runs its `rerere-gc` task — the run every
//! `commit`, `merge`, `am` and `fetch` starts in the background.
//!
//! git 2.56 gates the task on `rerere_gc_needed()` (rerere.c:1225-1273) with
//! `maintenance.rerere-gc.auto` defaulting to 512 (builtin/gc.c:397-408): only the
//! ids starting with `17` are sampled, each stale variant among them counts for
//! 256, and the task runs once the estimate reaches the limit. zvcs ran it
//! whenever `rr-cache` held anything, so a commit's background maintenance
//! pruned old resolutions stock leaves alone — including ones a following
//! `rerere gc` had been told to keep with `gc.rerere*=never`.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A repository with rerere enabled and a 400-day-old resolved record for
    /// each of `ids`.
    fn new(tag: &str, ids: &[&str]) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-maint-rerere-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let f = Fixture { root: root.canonicalize().unwrap() };
        f.git(&[], &["init", "-q", "."]);
        f.git(&[], &["config", "rerere.enabled", "true"]);
        let old = SystemTime::now() - Duration::from_secs(400 * 86400);
        for id in ids {
            let dir = f.root.join(".git/rr-cache").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            for name in ["preimage", "postimage"] {
                let path = dir.join(name);
                std::fs::write(&path, "x\n").unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_accessed(old).set_modified(old))
                    .unwrap();
            }
        }
        f
    }

    fn git(&self, config: &[&str], args: &[&str]) {
        let mut cmd = Command::new(BIN);
        for kv in config {
            cmd.args(["-c", kv]);
        }
        let out = cmd
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn auto(&self, config: &[&str]) -> Vec<String> {
        self.git(config, &["maintenance", "run", "--auto", "--quiet"]);
        let mut ids: Vec<String> = std::fs::read_dir(self.root.join(".git/rr-cache"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        ids.sort();
        ids
    }
}

const A17: &str = "1700000000000000000000000000000000000001";
const B17: &str = "1711111111111111111111111111111111111111";
const AA: &str = "aa00000000000000000000000000000000000000";
const BB: &str = "bb00000000000000000000000000000000000001";

/// One stale `17` record estimates 256, under the default 512: nothing runs.
#[test]
fn one_sampled_stale_record_stays_under_the_default() {
    let f = Fixture::new("one", &[A17]);
    assert_eq!(f.auto(&[]), vec![A17]);
}

/// Two stale `17` records reach 512: the task runs, and `rerere gc` prunes
/// every stale record, sampled or not.
#[test]
fn two_sampled_stale_records_reach_the_default() {
    let f = Fixture::new("two", &[A17, B17, AA]);
    assert_eq!(f.auto(&[]), Vec::<String>::new());
}

/// Records outside the `17` sample never count, however stale.
#[test]
fn records_outside_the_sample_are_not_counted() {
    let f = Fixture::new("other", &[AA, BB]);
    assert_eq!(f.auto(&[]), vec![AA, BB]);
}

/// The limit is a threshold, `0` never runs the task and a negative value
/// always does.
#[test]
fn the_limit_is_a_threshold_with_never_and_always() {
    let f = Fixture::new("limit", &[A17, B17, AA]);
    assert_eq!(f.auto(&["maintenance.rerere-gc.auto=600"]), vec![A17, B17, AA]);

    let f = Fixture::new("never", &[A17]);
    assert_eq!(f.auto(&["maintenance.rerere-gc.auto=0"]), vec![A17]);

    let f = Fixture::new("always", &[A17]);
    assert_eq!(f.auto(&["maintenance.rerere-gc.auto=-1"]), Vec::<String>::new());
}
