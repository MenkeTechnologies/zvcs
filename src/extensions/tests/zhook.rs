//! `zhook` set/show/list/test.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(["-c", "user.email=t@e.x", "-c", "user.name=t"]).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn zvcs(home: &Path, cwd: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(BIN).args(args).current_dir(cwd).env("ZVCS_HOME", home).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.success())
}

#[test]
fn zhook_set_show_list_test() {
    let root = std::env::temp_dir().join(format!("zvcs-zhook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "c0"]);

    let marker = root.join("hook-ran.txt");
    // set a hook that writes the event to a marker.
    let (_o, ok) = zvcs(&home, &repo, &["zhook", "set", "echo", "$ZVCS_EVENT", ">", marker.to_str().unwrap()]);
    assert!(ok, "zhook set failed");

    // show returns the command.
    let (show, _) = zvcs(&home, &repo, &["zhook", "show"]);
    assert!(show.contains("ZVCS_EVENT") && show.contains("hook-ran"), "zhook show:\n{show}");

    // list (after indexing) includes this repo.
    assert!(Command::new(BIN).args(["zreindex", repo.to_str().unwrap()]).current_dir(&repo).env("ZVCS_HOME", &home).status().unwrap().success());
    let (list, _) = zvcs(&home, &repo, &["zhook", "list"]);
    assert!(list.contains("repo"), "zhook list missing repo:\n{list}");

    // test fires it → marker written.
    let (_t, tok) = zvcs(&home, &repo, &["zhook", "test"]);
    assert!(tok, "zhook test failed");
    let contents = std::fs::read_to_string(&marker).unwrap_or_default();
    assert!(!contents.trim().is_empty(), "hook did not run (marker empty)");

    let _ = std::fs::remove_dir_all(&root);
}
