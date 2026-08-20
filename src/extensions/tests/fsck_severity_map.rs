//! `fsck.<msg-id>`, `fsck.skipList` and the one message id `core.bigFileThreshold`
//! decides — the severity map `git fsck` resolves before it checks anything.
//!
//! Every expectation here is a byte captured from stock git 2.55.0 run under this
//! harness's exact environment. Four claims:
//!
//!   1. **`gitmodulesLarge` is a real check, and a narrow one.** `fsck.c:1198`
//!      reports it only when the caller handed `fsck_blob()` a null buffer, and
//!      the only caller on this path that ever does is `read_loose_object()`
//!      (`object-file.c:1645`), for a *loose* *blob* whose size is strictly
//!      greater than `core.bigFileThreshold`. A packed blob is decoded in full,
//!      and so is a blob `fsck_finish()` reaches — `fsck_blobs()` always calls
//!      `odb_read_object()` (`fsck.c:1337`). So the same repository reports the
//!      id or does not depending on the threshold, on whether the blob is loose,
//!      and on whether the tree that names it was scanned first.
//!   2. **The severity a message is reported at is `fsck.<msg-id>`'s to choose**,
//!      and it decides the `error:`/`warning:` prefix *and* the exit code.
//!   3. **`fsck.skipList` drops every message about the object ids it names**,
//!      with `oidset_parse_file_carefully()`'s comment/blank/whitespace rules
//!      (`oidset.c:73`) and its two `die()`s.
//!   4. **Every id in `FOREACH_FSCK_MSG_ID` is configurable**, including the ones
//!      whose check this port does not perform: git validates the value for all
//!      of them and dies on a misspelled id, so accepting `fsck.badGpgsig=bogus`
//!      silently would be as wrong as rejecting `fsck.badGpgsig=warn`.
//!
//! The fixtures are built with the binary under test, so nothing here needs a
//! system `git` at run time.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The 500-byte-plus `.gitmodules` used throughout, and the two object ids a
/// repository holding it always has. Both are pure functions of the content, so
/// they are stable across machines and across the harness's fixed identity.
const BIG_GITMODULES_LEN: usize = 501;
const BIG_BLOB: &str = "b6fca446b502e862f37253b2bb8af2f22a451b02";
/// The tree naming it, whose fanout (`9d`) sorts *before* the blob's (`b6`), so
/// `fsck`'s object-directory scan reaches the tree first and the blob is linted
/// by the per-object pass — the one that can see a streamed buffer.
const TREE_BEFORE_BLOB: &str = "9d711675fd230b716ac366da27d514349217f309";

/// The same content plus a `-6` suffix, chosen so the blob's fanout (`b5`) sorts
/// *before* its tree's (`d2`). The blob is then scanned before anything has named
/// it, so `fsck_finish()` lints it instead — with a full buffer, and so silently.
const BIG_BLOB_FIRST: &str = "b5db0ab0d229e76dc34cd07a9a8cda854386f7b9";
const TREE_AFTER_BLOB: &str = "d2fd1dbdb3aec850f573b5a2ab86da9276324ad5";

/// Run the binary under test with a pinned identity, clock and environment.
fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TERM", "dumb")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run binary under test")
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "{args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// stderr and the exit code, which together are what `git fsck` communicates.
fn fsck(dir: &Path, home: &Path, config: &[&str]) -> (String, i32) {
    let mut args: Vec<&str> = config.to_vec();
    args.push("fsck");
    args.push("--no-progress");
    let out = run(dir, home, &args);
    (
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// A repository whose only content is a `.gitmodules` of
/// `BIG_GITMODULES_LEN - 1` `x`es, then `suffix`, then a newline.
fn repo_with_big_gitmodules(tag: &str, suffix: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fsck-sev-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let mut content = "x".repeat(BIG_GITMODULES_LEN - 1);
    content.push_str(suffix);
    content.push('\n');
    ok(&repo, &home, &["init", "-q", "-b", "main", "."]);
    std::fs::write(repo.join(".gitmodules"), &content).unwrap();
    ok(&repo, &home, &["add", ".gitmodules"]);
    ok(&repo, &home, &["commit", "-q", "-m", "gm"]);
    (repo, home)
}

/// Claim 1: what `gitmodulesLarge` fires on, and what it does not.
#[test]
fn gitmodules_large_needs_a_streamed_loose_blob() {
    let (repo, home) = repo_with_big_gitmodules("large", "");
    assert_eq!(
        ok(&repo, &home, &["rev-parse", "HEAD:.gitmodules"]).trim(),
        BIG_BLOB,
        "fixture drifted; the rest of this test pins ids derived from it"
    );
    assert_eq!(ok(&repo, &home, &["rev-parse", "HEAD^{tree}"]).trim(), TREE_BEFORE_BLOB);

    let reported = format!(
        "error in blob {BIG_BLOB}: gitmodulesLarge: .gitmodules too large to parse\n"
    );

    // git's default threshold is 512 MiB (`repo-settings.c:171`), so a 501-byte
    // blob is nowhere near it.
    assert_eq!(fsck(&repo, &home, &[]), (String::new(), 0));

    // `size > threshold`, strictly: 500 reports, 501 does not.
    assert_eq!(
        fsck(&repo, &home, &["-c", "core.bigFileThreshold=500"]),
        (reported.clone(), 1)
    );
    assert_eq!(
        fsck(&repo, &home, &["-c", "core.bigFileThreshold=501"]),
        (String::new(), 0)
    );

    // Once the blob is packed there is no streaming reader in the path at all:
    // `verify_packfile()` decodes every object in full.
    ok(&repo, &home, &["repack", "-adq"]);
    assert_eq!(
        fsck(&repo, &home, &["-c", "core.bigFileThreshold=500"]),
        (String::new(), 0),
        "a packed .gitmodules is never streamed, so the id cannot fire"
    );
}

/// Claim 1, second half: the id is scan-order dependent, because only the
/// per-object pass can see a null buffer.
#[test]
fn gitmodules_large_is_silent_when_the_blob_is_scanned_before_its_tree() {
    let (repo, home) = repo_with_big_gitmodules("order", "-6");
    assert_eq!(ok(&repo, &home, &["rev-parse", "HEAD:.gitmodules"]).trim(), BIG_BLOB_FIRST);
    assert_eq!(ok(&repo, &home, &["rev-parse", "HEAD^{tree}"]).trim(), TREE_AFTER_BLOB);
    assert!(
        BIG_BLOB_FIRST[..2] < TREE_AFTER_BLOB[..2],
        "the point of this fixture is that the blob's fanout sorts first"
    );

    // `fsck_loose()` reaches the blob before any tree has put it in
    // `gitmodules_found`, so `fsck_blob()` does nothing there; `fsck_finish()`
    // picks it up afterwards through `fsck_blobs()`, which reads it whole.
    assert_eq!(
        fsck(&repo, &home, &["-c", "core.bigFileThreshold=100"]),
        (String::new(), 0)
    );
}

/// Claim 2: `fsck.gitmodulesLarge` decides the prefix and the exit code, and an
/// unusable value is git's `fatal:` before a single object is read.
#[test]
fn fsck_msg_id_severity_decides_the_prefix_and_the_exit_code() {
    let (repo, home) = repo_with_big_gitmodules("severity", "");
    let threshold = ["-c", "core.bigFileThreshold=500"];
    let with = |value: &str| {
        let mut args = threshold.to_vec();
        let setting = format!("fsck.gitmodulesLarge={value}");
        args.push("-c");
        args.push(&setting);
        fsck(&repo, &home, &args)
    };

    assert_eq!(
        with("error"),
        (
            format!("error in blob {BIG_BLOB}: gitmodulesLarge: .gitmodules too large to parse\n"),
            1
        )
    );
    assert_eq!(
        with("warn"),
        (
            format!(
                "warning in blob {BIG_BLOB}: gitmodulesLarge: .gitmodules too large to parse\n"
            ),
            0
        ),
        "a demoted error must stop setting ERROR_OBJECT, not just change its prefix"
    );
    assert_eq!(with("ignore"), (String::new(), 0));

    // `parse_msg_type()` (`fsck.c:118`) compares the three names case-sensitively
    // and `die()`s on anything else.
    assert_eq!(with("bogus"), ("fatal: Unknown fsck message type: 'bogus'\n".into(), 128));
    assert_eq!(with("Warn"), ("fatal: Unknown fsck message type: 'Warn'\n".into(), 128));
}

/// Claim 3: `fsck.skipList` drops the finding, with git's parsing rules.
#[test]
fn skip_list_drops_messages_about_the_ids_it_names() {
    let (repo, home) = repo_with_big_gitmodules("skiplist", "");
    let reported = format!(
        "error in blob {BIG_BLOB}: gitmodulesLarge: .gitmodules too large to parse\n"
    );
    let list = repo.join("skip.txt");
    let with_list = |path: &Path| {
        let setting = format!("fsck.skipList={}", path.display());
        fsck(&repo, &home, &["-c", "core.bigFileThreshold=500", "-c", &setting])
    };

    // Without the list the finding stands, which is what makes the rest of this
    // test meaningful.
    assert_eq!(
        fsck(&repo, &home, &["-c", "core.bigFileThreshold=500"]),
        (reported.clone(), 1)
    );

    std::fs::write(&list, format!("{BIG_BLOB}\n")).unwrap();
    assert_eq!(with_list(&list), (String::new(), 0));

    // `oidset_parse_file_carefully()` (`oidset.c:88`): "Allow trailing comments,
    // leading whitespace (including before commits), and empty or whitespace
    // only lines."
    std::fs::write(&list, format!("# a comment\n\n  {BIG_BLOB}   # trailing\n")).unwrap();
    assert_eq!(with_list(&list), (String::new(), 0));

    // A list naming some *other* object leaves this one reported: the skip is
    // per object id, not a global off switch.
    std::fs::write(&list, format!("{TREE_BEFORE_BLOB}\n")).unwrap();
    assert_eq!(with_list(&list), (reported, 1));

    // `oidset.c:83` and `oidset.c:101`, both `die()`s.
    let missing = repo.join("nope.txt");
    assert_eq!(
        with_list(&missing),
        (
            format!("fatal: could not open object name list: {}\n", missing.display()),
            128
        )
    );
    std::fs::write(&list, "not-a-hash\n").unwrap();
    assert_eq!(with_list(&list), ("fatal: invalid object name: not-a-hash\n".into(), 128));
}

/// Claim 4: the ids whose check this port does not perform are still configurable
/// exactly as git's are.
///
/// These are the `msg_config_only!` rows. Each names a check that cannot fire
/// here — `badReftableTableName` needs a reftable backend, and the other six are
/// unreachable in git itself, each behind a parse that rejects the same buffer
/// first. What must still hold is that `fsck.<id>` behaves: a value git rejects
/// is rejected, a value git accepts is accepted, and neither is confused with the
/// misspelled id next to it.
#[test]
fn config_only_msg_ids_validate_exactly_as_git_does() {
    let (repo, home) = repo_with_big_gitmodules("configonly", "");
    const CONFIG_ONLY: &[&str] = &[
        "badGpgsig",
        "badHeaderContinuation",
        "badReftableTableName",
        "badTreeSha1",
        "emptyName",
        "missingTree",
        "unknownType",
    ];
    for id in CONFIG_ONLY {
        let bad = format!("fsck.{id}=bogus");
        assert_eq!(
            fsck(&repo, &home, &["-c", &bad]),
            ("fatal: Unknown fsck message type: 'bogus'\n".into(), 128),
            "fsck.{id} must reject a severity git rejects"
        );
        let good = format!("fsck.{id}=warn");
        assert_eq!(
            fsck(&repo, &home, &["-c", &good]),
            (String::new(), 0),
            "fsck.{id} must accept a severity git accepts"
        );
    }

    // `parse_msg_id()` (`fsck.c:79`) knows none of these, and `fsck_set_msg_type()`
    // (`fsck.c:162`) dies on the lowercased spelling of what it was given.
    for spelling in ["fsck.noSuchId=warn", "fsck.gitmoduleslarge2=warn", "fsck.badgpgsi=warn"] {
        let (stderr, code) = fsck(&repo, &home, &["-c", spelling]);
        assert_eq!(code, 128, "{spelling}");
        assert!(
            stderr.starts_with("fatal: Unhandled message id: "),
            "{spelling}: {stderr}"
        );
    }

    // The two `FSCK_FATAL` ids cannot be demoted at all (`fsck.c:176`), and the
    // message names the id in its lowercased form.
    assert_eq!(
        fsck(&repo, &home, &["-c", "fsck.nulInHeader=warn"]),
        ("fatal: Cannot demote nulinheader to warn\n".into(), 128)
    );
    assert_eq!(
        fsck(&repo, &home, &["-c", "fsck.unterminatedHeader=ignore"]),
        ("fatal: Cannot demote unterminatedheader to ignore\n".into(), 128)
    );
    // …but setting one to `error` is a no-op that must be accepted.
    assert_eq!(
        fsck(&repo, &home, &["-c", "fsck.nulInHeader=error"]),
        (String::new(), 0)
    );
}
