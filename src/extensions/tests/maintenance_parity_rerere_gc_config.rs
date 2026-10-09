//! The `rerere gc` child that `gc` and the `rerere-gc` maintenance task run reads the
//! configuration whether or not an `rr-cache` directory exists, so a `merge.conflictStyle` it
//! cannot use (or an unreadable `rerere.*` boolean) fails it: `gc` ends `fatal: failed to run
//! rerere` at 128 after the child's own lines, the task is `error: task 'rerere-gc' failed` and
//! `maintenance run` ends 1. `gc --auto` with nothing to do never starts the child.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

#[test]
fn a_config_the_rerere_child_refuses_fails_gc_and_the_maintenance_task() {
    let Some(stock) = stock_git::stock_git() else { return };
    let (s, z) = twin_repo::pair("rerere-gc-config", stock);
    for config in ["merge.conflictStyle=bogus", "rerere.enabled=bogus", "rerere.autoupdate=maybe"] {
        for args in [
            &["gc"][..],
            &["gc", "--auto"],
            &["maintenance", "run", "--task=rerere-gc"],
            &["maintenance", "run", "--task=gc"],
            &["maintenance", "run"],
        ] {
            let mut full = vec!["-c", config];
            full.extend_from_slice(args);
            let want = s.git(&full);
            assert_eq!(z.git(&full), want, "{config}: {args:?}");
        }
    }
    let want = s.git(&["-c", "merge.conflictStyle=bogus", "gc"]);
    assert_eq!(want.code, 128, "{want:?}");
    assert!(want.stderr.ends_with("fatal: failed to run rerere\n"), "{want:?}");
    let want = s.git(&["-c", "merge.conflictStyle=bogus", "maintenance", "run", "--task=rerere-gc"]);
    assert_eq!(want.code, 1, "{want:?}");
    assert!(want.stderr.ends_with("error: task 'rerere-gc' failed\n"), "{want:?}");
}
