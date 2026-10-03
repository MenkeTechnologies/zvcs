//! A write under `--blob` is refused before the blob is looked at.
//!
//! `check_write()` (builtin/config.c:812-822) dies with `writing config blobs is
//! not supported` (`editing blobs is not supported` for `--edit`) as the first
//! thing a writer does, so a `<blob>` that resolves to nothing adds no line of its
//! own. zvcs read the blob for every action and printed `error: unable to resolve
//! config blob '<blob>'` first. A reader still reports it. Expectations captured
//! from stock git 2.56.0.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env_remove("GIT_DIR")
        .output()
        .expect("run the binary under test");
    (String::from_utf8_lossy(&out.stderr).into_owned(), out.status.code().expect("no signal"))
}

#[test]
fn writers_refuse_an_unresolvable_blob_without_reading_it() {
    let root = std::env::temp_dir().join(format!("zvcs-config-blob-write-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);
    let write = "fatal: writing config blobs is not supported\n".to_string();
    for args in [
        &["config", "--blob=HEAD", "a.b", "c"][..],
        &["config", "--blob=HEAD", "--unset", "a.b"],
        &["config", "set", "--blob=HEAD", "a.b", "c"],
    ] {
        assert_eq!(git(&root, args), (write.clone(), 128), "{args:?}");
    }
    assert_eq!(
        git(&root, &["config", "--blob=HEAD", "--edit"]),
        ("fatal: editing blobs is not supported\n".to_string(), 128)
    );
    assert_eq!(
        git(&root, &["config", "--blob=HEAD", "a.b"]),
        ("error: unable to resolve config blob 'HEAD'\n".to_string(), 1)
    );
    let _ = std::fs::remove_dir_all(&root);
}
