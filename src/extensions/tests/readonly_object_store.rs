//! Writes into a read-only `.git/objects`, where git's answer depends on whether
//! the object is already there.
//!
//! `odb_source_loose_write_object()` returns early for an object some source
//! already holds (odb/source-loose.c:614-619), so `am -3`'s fake-ancestor
//! `write-tree` succeeds over trees that exist. A genuinely new object reaches
//! `start_loose_object_common()` and fails with git's "insufficient permission"
//! line (object-file.c:667-677); `apply --3way` ignores that failure, keeps the
//! computed id, and dies reading it back (`resolve_to()`, apply.c:3620-3622).
//!
//! The expected text is stock git 2.55's, captured by hand. A process that can
//! write through `chmod a-w` (root) skips the assertions.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) {
    let out = run(dir, home, args);
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn new_repo(root: &Path, home: &Path, name: &str, commits: &[&str]) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    ok(&repo, home, &["init", "-q", "-b", "main"]);
    ok(&repo, home, &["config", "user.email", "user@example.com"]);
    ok(&repo, home, &["config", "user.name", "User"]);
    for content in commits {
        std::fs::write(repo.join("f"), content).unwrap();
        ok(&repo, home, &["add", "f"]);
        ok(&repo, home, &["commit", "-q", "-m", content.lines().next().unwrap()]);
    }
    repo
}

/// A scratch root holding `src`, whose last commit turns `a b c` into `a B c`,
/// and that commit as `p.mbox`. The post-image blob exists only in `src`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-roodb-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let src = new_repo(&root, &home, "src", &["a\nb\nc\n", "a\nB\nc\n"]);
    let mbox = run(&src, &home, &["format-patch", "-1", "--stdout"]);
    assert!(mbox.status.success());
    std::fs::write(root.join("p.mbox"), &mbox.stdout).unwrap();
    (root, home)
}

/// `chmod -R a-w`/`u+w` over `.git/objects`.
fn set_writable(objects: &Path, writable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mut stack = vec![objects.to_path_buf()];
    while let Some(p) = stack.pop() {
        let md = std::fs::symlink_metadata(&p).unwrap();
        let mode = md.permissions().mode();
        let mode = if writable { mode | 0o200 } else { mode & !0o222 };
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        if md.is_dir() {
            stack.extend(std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
        }
    }
}

/// Make `.git/objects` read-only, run `f`, restore permissions, and report
/// whether the store was actually unwritable for this process.
fn with_readonly_store<T>(repo: &Path, f: impl FnOnce() -> T) -> Option<T> {
    let objects = repo.join(".git/objects");
    set_writable(&objects, false);
    let enforced = std::fs::File::create(objects.join("probe")).is_err();
    let result = enforced.then(f);
    set_writable(&objects, true);
    let _ = std::fs::remove_file(objects.join("probe"));
    result
}

#[test]
fn am_3way_reuses_existing_fake_ancestor_tree_and_stops_at_hand_edit() {
    let (root, home) = fixture("am");
    let repo = new_repo(&root, &home, "r", &["a\nb\nc\n", "a\nb\nc\nd\n", "zzz\n"]);
    let mbox = root.join("p.mbox");
    let Some(out) = with_readonly_store(&repo, || run(&repo, &home, &["am", "-3", mbox.to_str().unwrap()]))
    else {
        return;
    };
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Applying: a\nUsing index info to reconstruct a base tree...\nM\tf\nPatch failed at 0001 a\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: insufficient permission for adding an object to repository database .git/objects\n\
         error: unable to create backing store for newly created file f\n\
         error: Did you hand edit your patch?\n\
         It does not apply to blobs recorded in its index.\n\
         hint: Use 'git am --show-current-patch=diff' to see the failed patch\n\
         hint: When you have resolved this problem, run \"git am --continue\".\n\
         hint: If you prefer to skip this patch, run \"git am --skip\" instead.\n\
         hint: To restore the original branch and stop patching, run \"git am --abort\".\n\
         hint: Disable this message with \"git config set advice.mergeConflict false\"\n"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn apply_3way_dies_reading_back_a_post_image_it_could_not_write() {
    let (root, home) = fixture("apply");
    let repo = new_repo(&root, &home, "r", &["a\nb\nc\n"]);
    let mbox = root.join("p.mbox");
    let Some(out) = with_readonly_store(&repo, || run(&repo, &home, &["apply", "--3way", mbox.to_str().unwrap()]))
    else {
        return;
    };
    assert_eq!(out.status.code(), Some(128));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: insufficient permission for adding an object to repository database .git/objects\n\
         fatal: unable to read blob object 7be73ce3c1b1cdaea86e8168dfee8575175953bf\n"
    );
    assert_eq!(std::fs::read(repo.join("f")).unwrap(), b"a\nb\nc\n");
    let _ = std::fs::remove_dir_all(&root);
}
