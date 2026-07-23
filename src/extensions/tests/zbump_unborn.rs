//! One submodule with an unborn HEAD must not abort the whole `zbump` pass — it
//! is refused per-path and every other submodule is still processed.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(["-c", "user.email=t@e.x", "-c", "user.name=t", "-c", "protocol.file.allow=always"])
            .args(args)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@e.x")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@e.x")
            .current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn mk_src(root: &Path, name: &str) -> std::path::PathBuf {
    let s = root.join(name);
    std::fs::create_dir_all(&s).unwrap();
    git(&s, &["init", "-q", "-b", "main"]);
    std::fs::write(s.join("f.txt"), b"1\n").unwrap();
    git(&s, &["add", "f.txt"]);
    git(&s, &["commit", "-q", "-m", "c0"]);
    s
}

#[test]
fn unborn_submodule_head_does_not_abort_bump() {
    let root = std::env::temp_dir().join(format!("zvcs-zbumpunborn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");

    // Two submodule sources; "aaa" sorts before "zzz" so aaa is iterated first.
    let aaa_src = mk_src(&root, "aaa_src");
    let zzz_src = mk_src(&root, "zzz_src");

    let sup = root.join("super");
    std::fs::create_dir_all(&sup).unwrap();
    git(&sup, &["init", "-q", "-b", "main"]);
    git(&sup, &["commit", "--allow-empty", "-q", "-m", "p0"]);
    git(&sup, &["submodule", "add", "-q", aaa_src.to_str().unwrap(), "aaa"]);
    git(&sup, &["submodule", "add", "-q", zzz_src.to_str().unwrap(), "zzz"]);
    git(&sup, &["commit", "-q", "-m", "add subs"]);

    // Advance "zzz" so it is a bump candidate (ahead of the recorded gitlink).
    std::fs::write(sup.join("zzz/f.txt"), b"2\n").unwrap();
    git(&sup.join("zzz"), &["commit", "-qam", "c1"]);

    // Make "aaa" HEAD unborn (orphan branch, no commit yet).
    git(&sup.join("aaa"), &["checkout", "-q", "--orphan", "void"]);

    let out = Command::new(BIN).args(["zbump"]).current_dir(&sup).env("ZVCS_HOME", &home).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    // aaa is refused for the unborn HEAD...
    assert!(stdout.contains("aaa") && stdout.to_lowercase().contains("unborn"), "aaa should be refused as unborn:\n{stdout}");
    // ...and zzz (iterated AFTER aaa) is still processed — the pass did not abort.
    assert!(stdout.contains("zzz"), "zzz must still be processed despite aaa being unborn:\n{stdout}");

    let _ = std::fs::remove_dir_all(&root);
}
