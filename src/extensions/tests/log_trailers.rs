//! `%(trailers[:<options>])` in a `--format=` string, pinned against stock git
//! 2.50.1.
//!
//! The placeholder is the only parenthesised one that is not a colour request, so
//! it has to be recognised before `%C(...)` is — and the two traps in its output
//! are easy to get backwards:
//!
//!   * with no `separator=`, each trailer is *terminated* by a newline; with one,
//!     the separator *joins* them and no trailing copy is emitted;
//!   * `key=` is matched case-insensitively, tolerates the separator being written
//!     into the key, and turns `only` on by itself, so the block's non-trailer
//!     lines disappear with it.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .expect("run binary")
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let o = run(dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn fmt(dir: &Path, home: &Path, spec: &str) -> String {
    String::from_utf8_lossy(&run(dir, home, &["log", "-1", &format!("--format={spec}")]).stdout)
        .into_owned()
}

#[test]
fn trailers_placeholder_matches_git() {
    let root = std::env::temp_dir().join(format!("zvcs-logtrailers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "t@e.x"]);
    git(&repo, &home, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x\n").unwrap();
    git(&repo, &home, &["add", "f"]);
    // A trailer block with a non-trailer line in it and one folded continuation,
    // which is what `only` and `unfold` are there to deal with.
    let message = "subject\n\
                   \n\
                   body\n\
                   \n\
                   loose line in the block\n\
                   Signed-off-by: Ann Example <ann@example.com>\n\
                   \x20 folded onto the previous trailer\n\
                   Acked-by: Cy Example <cy@example.com>\n";
    std::fs::write(root.join("msg"), message).unwrap();
    git(&repo, &home, &["commit", "-q", "-F", root.join("msg").to_str().unwrap()]);

    // The whole block, non-trailer line included, each entry newline-terminated.
    assert_eq!(
        fmt(&repo, &home, "%(trailers)"),
        "loose line in the block\n\
         Signed-off-by: Ann Example <ann@example.com>\n\
         \x20 folded onto the previous trailer\n\
         Acked-by: Cy Example <cy@example.com>\n\n"
    );
    // `only` drops the line that is not a trailer.
    assert_eq!(
        fmt(&repo, &home, "%(trailers:only)"),
        "Signed-off-by: Ann Example <ann@example.com>\n\
         \x20 folded onto the previous trailer\n\
         Acked-by: Cy Example <cy@example.com>\n\n"
    );
    // `unfold` pulls the continuation back onto its trailer.
    assert_eq!(
        fmt(&repo, &home, "%(trailers:only,unfold)"),
        "Signed-off-by: Ann Example <ann@example.com> folded onto the previous trailer\n\
         Acked-by: Cy Example <cy@example.com>\n\n"
    );
    // `key=` implies `only`, matches case-insensitively, and accepts the key
    // written with its separator.
    assert_eq!(
        fmt(&repo, &home, "%(trailers:key=acked-by)"),
        "Acked-by: Cy Example <cy@example.com>\n\n"
    );
    assert_eq!(
        fmt(&repo, &home, "%(trailers:key=Acked-by:)"),
        "Acked-by: Cy Example <cy@example.com>\n\n"
    );
    assert_eq!(fmt(&repo, &home, "%(trailers:key=Nope)"), "\n", "no match prints nothing");
    // A given separator joins instead of terminating, so there is no trailing one.
    assert_eq!(
        fmt(&repo, &home, "%(trailers:only,unfold,keyonly,separator=%x2C)"),
        "Signed-off-by,Acked-by\n"
    );
    assert_eq!(
        fmt(&repo, &home, "%(trailers:key=Acked-by,valueonly)"),
        "Cy Example <cy@example.com>\n\n"
    );
    assert_eq!(
        fmt(&repo, &home, "%(trailers:key=Acked-by,key_value_separator==)"),
        "Acked-by=Cy Example <cy@example.com>\n\n"
    );
    // An option git does not know makes the placeholder print literally rather
    // than failing the command.
    assert_eq!(fmt(&repo, &home, "%(trailers:bogus)"), "%(trailers:bogus)\n");

    let _ = std::fs::remove_dir_all(&root);
}
