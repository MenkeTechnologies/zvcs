//! `git for-each-repo` runs each child as `git -C <path> <args>`
//! (builtin/for-each-repo.c:17-32), so every failure to enter the path is the
//! child's own `fatal: cannot change to '<path>': <strerror>` at 128 (git.c:321-323)
//! — a regular file as well as a missing directory, where zvcs printed
//! `zvcs: for-each-repo: Not a directory (os error 20)` at 1.
//!
//! A value `interpolate_path()` answers NULL for, like `~nosuchuser/r`, ends
//! `strvec_pushl(&child.args, "-C", abspath, NULL)` at the `-C`, so the child
//! takes the first argument as its directory; zvcs stopped on the interpolation
//! error. The child's environment is `sanitize_repo_env()`'d, so a `GIT_DIR` of
//! the caller does not leak into it.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use stock_git::stock_git;

#[test]
fn children_are_run_through_dash_c() {
    let Some(stock) = stock_git() else { return };
    let (s, z) = twin_repo::pair("for-each-repo-dash-c", stock);
    for side in [&s, &z] {
        side.write("file", "x\n");
    }
    let file = |side: &twin_repo::Side| format!("x.y={}", side.repo().join("file").display());
    let repo = |side: &twin_repo::Side| format!("x.y={}", side.repo().display());
    let cases: &[&dyn Fn(&twin_repo::Side) -> (Vec<(String, String)>, Vec<String>)] = &[
        &|side| (vec![], vec!["-c".into(), file(side), "for-each-repo".into(), "--config=x.y".into(), "rev-parse".into()]),
        &|side| {
            let args = ["--keep-going", "--config=x.y", "rev-parse", "--git-dir"];
            let mut argv = vec!["-c".into(), file(side), "-c".into(), repo(side), "for-each-repo".into()];
            argv.extend(args.iter().map(|a| a.to_string()));
            (vec![], argv)
        },
        &|_| (vec![], ["-c", "x.y=~nosuchuser/r", "for-each-repo", "--config=x.y", "rev-parse", "--git-dir"].map(String::from).to_vec()),
        // Run from `repo` through its `GIT_DIR`; the child in `other` finds its
        // own `.git`, not the caller's.
        &|side| {
            let env = vec![("GIT_DIR".to_string(), side.repo().join(".git").display().to_string())];
            let other = format!("x.y={}", side.root.join("other").display());
            (env, vec!["-c".into(), other, "for-each-repo".into(), "--config=x.y".into(), "rev-parse".into(), "--git-dir".into()])
        },
    ];
    for side in [&s, &z] {
        side.run_in(&side.root, &[], &["init", "-q", "other"]);
    }
    for case in cases {
        let run = |side: &twin_repo::Side| {
            let (env, argv) = case(side);
            let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            side.run_in(&side.root, &env, &argv)
        };
        assert_eq!(run(&z), run(&s));
    }
}
