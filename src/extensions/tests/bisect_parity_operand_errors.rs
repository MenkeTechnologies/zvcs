//! How `git bisect` refuses an operand.
//!
//! `bisect_state()` (builtin/bisect.c:1099-1113) separates two failures: a name
//! `repo_get_oid()` cannot resolve is `error: Bad rev input: <name>` and exit 1,
//! but one that resolves to something `lookup_commit_reference()` cannot make a
//! commit of — a blob, a tree, an id the odb lacks — is `die(_("Bad rev input
//! (not a commit): %s"))` after the lookup's own `object %s is a %s, not a
//! commit`. Before either, `repo_get_oid()` itself dies on an `@{upstream}` or
//! `@{push}` mark that names nothing (object-name.c), in `bisect start` as much
//! as in `good`/`bad`/`skip`. zvcs answered all of them `Bad rev input` at exit
//! 1, and `bisect start main@{upstream}` started a session. Expectations
//! measured from stock git 2.56.0.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run the binary under test");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

#[test]
fn operands_that_are_not_commits_die_and_unknown_ones_fail() {
    let root = std::env::temp_dir().join(format!("zvcs-bisect-operands-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("f"), "1\n").unwrap();
    git(&root, &["add", "f"]);
    git(&root, &["commit", "-q", "-m", "one"]);

    let upstream = "fatal: no upstream configured for branch 'main'\n";
    assert_eq!(git(&root, &["bisect", "start", "main@{upstream}"]), (String::new(), upstream.into(), 128));
    assert!(!root.join(".git/BISECT_START").exists());

    assert_eq!(git(&root, &["bisect", "start"]).2, 0);
    assert_eq!(git(&root, &["bisect", "bad", "@{push}"]), (String::new(), upstream.into(), 128));
    assert_eq!(git(&root, &["bisect", "skip", "@{u}"]), (String::new(), upstream.into(), 128));

    let blob = git(&root, &["rev-parse", ":0:f"]).0.trim().to_string();
    assert_eq!(
        git(&root, &["bisect", "skip", ":0:f"]),
        (
            String::new(),
            format!("error: object {blob} is a blob, not a commit\nfatal: Bad rev input (not a commit): :0:f\n"),
            128
        )
    );
    let missing = "0000000000000000000000000000000000000001";
    assert_eq!(
        git(&root, &["bisect", "good", missing]),
        (String::new(), format!("fatal: Bad rev input (not a commit): {missing}\n"), 128)
    );
    assert_eq!(
        git(&root, &["bisect", "good", "nosuch"]),
        (String::new(), "error: Bad rev input: nosuch\n".into(), 1)
    );
    let _ = std::fs::remove_dir_all(&root);
}
