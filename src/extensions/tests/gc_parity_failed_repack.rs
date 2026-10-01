//! What `gc` says when its `repack` child fails.
//!
//! git 2.56.0 moved the repack out of `cmd_gc()` into `odb_optimize()`, which
//! reports a failed child with `error("failed to run %s", repack_cmd.args.v[0])`
//! (odb/source-files.c:730-733) after `run_command()` has already cleared the
//! argument vector, so the line reads `error: failed to run (null)`. `cmd_gc()`
//! then exits 128 through `die(NULL)` (builtin/gc.c:725-726), which prints
//! nothing (usage.c:69-72). 2.55.0 said `fatal: failed to run repack` instead,
//! and zvcs still did.
//!
//! Expectations measured from stock git 2.56.0 under the same environment.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-gc-failed-repack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.run(&["init", "-q", "."]);
        std::fs::write(f.work.join("a"), "a\n").unwrap();
        f.run(&["add", "a"]);
        f.run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", "one"]);
        f
    }

    /// A branch naming an object the repository does not have, which the
    /// `pack-objects` traversal under `repack` dies on.
    fn damage(&self) {
        std::fs::write(
            self.work.join(".git/refs/heads/dangling"),
            "1234567890123456789012345678901234567890\n",
        )
        .unwrap();
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }
}

const DAMAGED: &str = "error: refs/heads/dangling does not point to a valid object!\n\
                       fatal: bad object refs/heads/dangling\n\
                       error: failed to run (null)\n";

#[test]
fn a_repack_that_dies_on_a_bad_ref_is_reported_with_a_null_command_name() {
    let f = Fixture::new("damaged");
    f.damage();
    assert_eq!(f.run(&["gc", "--quiet"]), (String::new(), DAMAGED.to_string(), 128));
    assert_eq!(f.run(&["gc", "--prune=now", "--quiet"]), (String::new(), DAMAGED.to_string(), 128));
}

#[test]
fn the_gc_task_of_maintenance_relays_the_childs_lines_and_fails_the_task() {
    let f = Fixture::new("maintenance");
    f.damage();
    assert_eq!(
        f.run(&["maintenance", "run", "--task=gc", "--no-detach", "--quiet"]),
        (String::new(), format!("{DAMAGED}error: task 'gc' failed\n"), 1)
    );
}

#[test]
fn a_pack_config_value_the_child_dies_on_gets_the_same_trailing_line() {
    // `pack.useBitmaps` is read by the `pack-objects` grandchild, so the die is
    // the child's and `odb_optimize()` still adds its line after it.
    let f = Fixture::new("bitmaps");
    assert_eq!(
        f.run(&["-c", "pack.useBitmaps=bogus", "gc", "-q"]),
        (
            String::new(),
            "fatal: bad boolean config value 'bogus' for 'pack.usebitmaps'\n\
             error: failed to run (null)\n"
                .to_string(),
            128
        )
    );
}
