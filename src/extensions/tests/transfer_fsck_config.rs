//! `fetch.fsckObjects`, `fetch.fsck.<msg-id>`, `fetch.fsck.skipList` and the
//! `receive.fsck.*` family — the two transfer-side copies of the fsck severity
//! map.
//!
//! git keeps three independent families and says so (`git help config`, under
//! `fsck.<msg-id>`): "the `receive.fsck.<msg-id>` and `fetch.fsck.<msg-id>`
//! variables will not fall back on the `fsck.<msg-id>` configuration if they
//! aren't set." Each is read by its own callback — `git_fsck_config()`
//! (`fsck.c:1440`), `receive_pack_config()` (`builtin/receive-pack.c:174`) and
//! `fetch_pack_fsck_config()` (`fetch-pack.c:1954`) — and the two transfer ones
//! differ from the `fsck.` one in three ways this file pins:
//!
//!   1. **They validate every id, but diagnose an unknown one softly.** A
//!      misspelled `fsck.<x>` kills `git fsck`; the same under `receive.fsck.` /
//!      `fetch.fsck.` is a warning, and the two warnings differ by one byte —
//!      `builtin/receive-pack.c:193` writes `skipping`, `fetch-pack.c:1978`
//!      writes `Skipping`.
//!   2. **A bad *value* is fatal on all three**, because `is_valid_msg_type()`
//!      calls `parse_msg_type()`, which `die()`s (`fsck.c:127`) — and it is fatal
//!      whether or not the check will actually run.
//!   3. **The demote rule and the skip-list read happen in the child.**
//!      `fsck_set_msg_type()` and `oidset_parse_file()` are only reached inside
//!      the `index-pack`/`unpack-objects` the transfer starts, so they are
//!      silent until `fetch.fsckObjects` / `receive.fsckObjects` turns the check
//!      on, and then they arrive with the child's own `index-pack failed`.
//!
//! Every expectation is a byte captured from stock git 2.55.0 under this
//! harness's environment; the fixtures are built with the binary under test.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The commit built by [`broken_source`]: a valid tree, a committer line that is
/// fine, and an author line whose date is zero-padded. One defect, reported once
/// — git reports `zeroPaddedDate` separately for the author and the committer,
/// so a commit with both padded would produce two lines under `warn`.
const BAD_COMMIT: &str = "23e50ca4b4126367fb2f689204dd75c2aca4df8f";
const BAD_COMMIT_TREE: &str = "2e81171448eb9f2ee3821e3d447aa6b2fe3ddba1";
const ZERO_PADDED: &str = "zeroPaddedDate: invalid author/committer line - zero-padded date";

/// The 501-byte `.gitmodules` blob of [`big_gitmodules_source`].
const BIG_BLOB: &str = "b6fca446b502e862f37253b2bb8af2f22a451b02";
const GITMODULES_LARGE: &str = "gitmodulesLarge: .gitmodules too large to parse";

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    bindir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-xfer-fsck-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let fx = Fixture {
            home: root.join("home"),
            bindir: root.join("bin"),
            root,
        };
        std::fs::create_dir_all(&fx.home).unwrap();
        std::fs::create_dir_all(&fx.bindir).unwrap();
        // A local push looks `git-receive-pack` up on `PATH`; serve it, and the
        // fetch service beside it, with the binary under test.
        for name in ["git", "git-receive-pack", "git-upload-pack"] {
            std::os::unix::fs::symlink(BIN, fx.bindir.join(name)).unwrap();
        }
        fx
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bindir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(BIN)
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
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

    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.run(dir, args);
        assert!(
            out.status.success(),
            "{args:?} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn stdin_run(&self, dir: &Path, args: &[&str]) -> Output {
        self.run(dir, args)
    }

    /// A fresh empty repository to fetch into.
    fn empty(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        self.ok(&dir, &["init", "-q", "-b", "main", "."]);
        dir
    }

    fn bare(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        self.ok(&dir, &["init", "-q", "--bare", "-b", "main", "."]);
        dir
    }

    /// A repository whose `refs/heads/main` is a commit `fsck` reports
    /// `zeroPaddedDate` for, reachable through a tree and a blob that are fine.
    fn broken_source(&self) -> PathBuf {
        let src = self.root.join("broken-src");
        std::fs::create_dir_all(&src).unwrap();
        self.ok(&src, &["init", "-q", "-b", "main", "."]);
        std::fs::write(src.join("a.txt"), "hello\n").unwrap();
        self.ok(&src, &["add", "a.txt"]);
        let tree = self.ok(&src, &["write-tree"]).trim().to_string();
        assert_eq!(tree, BAD_COMMIT_TREE);
        let raw = format!(
            "tree {tree}\n\
             author Test <test@example.com> 0000000000 +0000\n\
             committer Test <test@example.com> 1700000000 +0000\n\
             \n\
             broken\n"
        );
        let raw_path = self.root.join("commit.raw");
        std::fs::write(&raw_path, raw).unwrap();
        let commit = self
            .ok(
                &src,
                &["hash-object", "-w", "-t", "commit", "--literally", raw_path.to_str().unwrap()],
            )
            .trim()
            .to_string();
        assert_eq!(commit, BAD_COMMIT, "fixture drifted");
        self.ok(&src, &["update-ref", "refs/heads/main", &commit]);
        src
    }

    /// A repository holding a 501-byte `.gitmodules`, which is over any
    /// `core.bigFileThreshold` the tests set.
    fn big_gitmodules_source(&self) -> PathBuf {
        let src = self.root.join("gm-src");
        std::fs::create_dir_all(&src).unwrap();
        self.ok(&src, &["init", "-q", "-b", "main", "."]);
        let mut content = "x".repeat(500);
        content.push('\n');
        std::fs::write(src.join(".gitmodules"), content).unwrap();
        self.ok(&src, &["add", ".gitmodules"]);
        self.ok(&src, &["commit", "-q", "-m", "gm"]);
        assert_eq!(self.ok(&src, &["rev-parse", "HEAD:.gitmodules"]).trim(), BIG_BLOB);
        src
    }
}

/// stderr and the exit code of a `fetch <src> main` run with `config` prepended.
fn fetch(fx: &Fixture, dst: &Path, src: &Path, config: &[&str]) -> (String, i32) {
    let mut args: Vec<&str> = config.to_vec();
    args.push("fetch");
    let src = src.to_str().unwrap();
    args.push(src);
    args.push("main");
    let out = fx.run(dst, &args);
    (
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn has_pack(repo: &Path) -> bool {
    std::fs::read_dir(repo.join(".git/objects/pack"))
        .map(|d| {
            d.filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().ends_with(".pack"))
        })
        .unwrap_or(false)
}

/// `fetch.fsckObjects` turns the check on, `transfer.fsckObjects` is its
/// fallback, and a failure leaves the fetch with nothing.
#[test]
fn fetch_fsck_objects_rejects_a_broken_object_and_stores_nothing() {
    let fx = Fixture::new("reject");
    let src = fx.broken_source();

    // Without the check the fetch succeeds, and — being three objects, well under
    // `fetch.unpackLimit` — leaves them loose rather than packed.
    let dst = fx.empty("d-default");
    let (stderr, code) = fetch(&fx, &dst, &src, &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("-> FETCH_HEAD"), "{stderr}");
    assert!(!has_pack(&dst), "an unchecked small fetch is exploded loose");

    let failure = format!(
        "error: object {BAD_COMMIT}: {ZERO_PADDED}\n\
         fatal: fsck error in packed object\n\
         fatal: index-pack failed\n"
    );
    for key in ["fetch.fsckObjects=true", "transfer.fsckObjects=true"] {
        let dst = fx.empty("d-fail");
        assert_eq!(
            fetch(&fx, &dst, &src, &["-c", key]),
            (failure.clone(), 128),
            "{key}"
        );
        assert_eq!(fx.ok(&dst, &["for-each-ref"]), "", "{key}: no ref may be stored");
        assert!(!dst.join(".git/FETCH_HEAD").exists(), "{key}: no FETCH_HEAD either");
        assert!(!has_pack(&dst), "{key}: index-pack dies before installing the pack");
    }
}

/// `fetch.fsck.<msg-id>` chooses the severity, and `fsck.<msg-id>` is not
/// consulted on this path at all.
#[test]
fn fetch_fsck_msg_id_severity_is_its_own_family() {
    let fx = Fixture::new("family");
    let src = fx.broken_source();
    let on = "fetch.fsckObjects=true";

    // Demoted to `ignore`: the fetch completes, and — because the check forced
    // `index-pack` rather than `unpack-objects` — the pack stays.
    let dst = fx.empty("d-ignore");
    let (stderr, code) = fetch(
        &fx,
        &dst,
        &src,
        &["-c", on, "-c", "fetch.fsck.zeroPaddedDate=ignore"],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains(ZERO_PADDED), "{stderr}");
    assert!(has_pack(&dst), "with the check on, fetch-pack always runs index-pack");

    // Demoted to `warn`: reported, but the fetch stands.
    let dst = fx.empty("d-warn");
    let (stderr, code) = fetch(
        &fx,
        &dst,
        &src,
        &["-c", on, "-c", "fetch.fsck.zeroPaddedDate=warn"],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.starts_with(&format!("warning: object {BAD_COMMIT}: {ZERO_PADDED}\n")),
        "{stderr}"
    );

    // The `fsck.` family does not reach the fetch: setting it to `ignore` leaves
    // the fetch failing exactly as it did unconfigured.
    let dst = fx.empty("d-noleak");
    let (stderr, code) = fetch(
        &fx,
        &dst,
        &src,
        &["-c", on, "-c", "fsck.zeroPaddedDate=ignore"],
    );
    assert_eq!(code, 128, "{stderr}");
    assert!(stderr.contains(&format!("error: object {BAD_COMMIT}: {ZERO_PADDED}")), "{stderr}");

    // `fetch.fsck.skipList` drops it by object id.
    let list = fx.root.join("skip.txt");
    std::fs::write(&list, format!("{BAD_COMMIT}\n")).unwrap();
    let setting = format!("fetch.fsck.skipList={}", list.display());
    let dst = fx.empty("d-skip");
    let (stderr, code) = fetch(&fx, &dst, &src, &["-c", on, "-c", &setting]);
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains(ZERO_PADDED), "{stderr}");
}

/// Where each `fetch.fsck.*` configuration failure surfaces: the value in the
/// fetch process, the demote rule and the skip-list read inside its child.
#[test]
fn fetch_fsck_config_failures_land_where_git_puts_them() {
    let fx = Fixture::new("cfgfail");
    let src = fx.broken_source();
    let on = "fetch.fsckObjects=true";

    // A bad value is fatal even with the check off — `is_valid_msg_type()` runs
    // over every `fetch.fsck.` variable as the config is read.
    let dst = fx.empty("d-badvalue");
    assert_eq!(
        fetch(&fx, &dst, &src, &["-c", "fetch.fsck.zeroPaddedDate=bogus"]),
        ("fatal: Unknown fsck message type: 'bogus'\n".into(), 128)
    );

    // The demote rule lives in `fsck_set_msg_type()`, which only the child calls:
    // silent with the check off, and the child's failure with it on.
    let dst = fx.empty("d-demote-off");
    let (stderr, code) = fetch(&fx, &dst, &src, &["-c", "fetch.fsck.nulInHeader=warn"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("-> FETCH_HEAD"), "{stderr}");

    let dst = fx.empty("d-demote-on");
    assert_eq!(
        fetch(&fx, &dst, &src, &["-c", on, "-c", "fetch.fsck.nulInHeader=warn"]),
        (
            "fatal: Cannot demote nulinheader to warn\nfatal: index-pack failed\n".into(),
            128
        )
    );

    // Same for an unreadable skip list — `oidset_parse_file()` is reached from
    // `fsck_set_msg_types()`, in the child.
    let missing = fx.root.join("nope.txt");
    let setting = format!("fetch.fsck.skipList={}", missing.display());
    let dst = fx.empty("d-skip-off");
    let (stderr, code) = fetch(&fx, &dst, &src, &["-c", &setting]);
    assert_eq!(code, 0, "{stderr}");

    let dst = fx.empty("d-skip-on");
    assert_eq!(
        fetch(&fx, &dst, &src, &["-c", on, "-c", &setting]),
        (
            format!(
                "fatal: could not open object name list: {}\nfatal: index-pack failed\n",
                missing.display()
            ),
            128
        )
    );
}

/// The one-byte difference between the two transfer families' unknown-id
/// warnings, and the fact that neither is fatal.
#[test]
fn unknown_msg_id_warns_softly_and_the_two_families_spell_it_differently() {
    let fx = Fixture::new("unknown");
    let src = fx.broken_source();

    // `fetch-pack.c:1978`: capital `S`. The warning precedes the check's own
    // output, because the configuration is read before the pack is asked for.
    let dst = fx.empty("d-unknown");
    let (stderr, code) = fetch(
        &fx,
        &dst,
        &src,
        &["-c", "fetch.fsckObjects=true", "-c", "fetch.fsck.noSuchId=warn"],
    );
    assert_eq!(
        (stderr, code),
        (
            format!(
                "warning: Skipping unknown msg id 'nosuchid'\n\
                 error: object {BAD_COMMIT}: {ZERO_PADDED}\n\
                 fatal: fsck error in packed object\n\
                 fatal: index-pack failed\n"
            ),
            128
        )
    );

    // `builtin/receive-pack.c:193`: lower-case `s`, and the advertisement is
    // still written.
    let bare = fx.bare("bare-unknown");
    fx.ok(&bare, &["config", "receive.fsck.noSuchId", "warn"]);
    let out = fx.stdin_run(&bare, &["receive-pack", bare.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("warning: skipping unknown msg id 'nosuchid'\n"),
        "{stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).is_empty(),
        "a soft warning must not suppress the advertisement"
    );
}

/// `receive.fsck.<msg-id>` is read and validated for *every* id in the table,
/// including the ones whose check can never fire on a transfer.
///
/// git's config callback does not consult reachability: it calls
/// `is_valid_msg_type()` on whatever follows `receive.fsck.`, and
/// `parse_msg_type()` dies on a value it does not know. Leaving one of these ids
/// unread would silently accept a value git rejects.
#[test]
fn receive_fsck_validates_ids_whose_check_a_push_cannot_reach() {
    let fx = Fixture::new("recvvalid");
    let bare = fx.bare("bare-valid");
    // Three groups, none of which a `receive-pack` can ever report:
    //   * ids only `git mktag` reaches, because every other entry point parses
    //     the tag before fscking it;
    //   * ids only the reference-database walk reaches, which fscks the local
    //     repository's ref files rather than what the pusher sent;
    //   * ids whose check is not performed by this port at all.
    const UNREACHABLE_ON_A_PUSH: &[&str] = &[
        "missingObject",
        "badObjectSha1",
        "missingTypeEntry",
        "missingType",
        "badType",
        "missingTagEntry",
        "missingTag",
        "badRefName",
        "badRefFiletype",
        "badRefContent",
        "badRefOid",
        "badHeadTarget",
        "badReferentName",
        "refMissingNewline",
        "trailingRefContent",
        "symlinkRef",
        "symrefTargetIsNotARef",
        "badPackedRefHeader",
        "badPackedRefEntry",
        "packedRefEntryNotTerminated",
        "packedRefUnsorted",
        "emptyPackedRefsFile",
        "badGpgsig",
        "badHeaderContinuation",
        "badReftableTableName",
        "badTreeSha1",
        "emptyName",
        "missingTree",
        "unknownType",
        "gitmodulesLarge",
    ];
    let dir = bare.to_str().unwrap().to_string();
    for id in UNREACHABLE_ON_A_PUSH {
        let key = format!("receive.fsck.{id}");
        fx.ok(&bare, &["config", &key, "bogus"]);
        let out = fx.stdin_run(&bare, &["receive-pack", &dir]);
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "fatal: Unknown fsck message type: 'bogus'\n",
            "{key}=bogus must be diagnosed exactly as git diagnoses it"
        );
        assert_eq!(out.status.code(), Some(128), "{key}");
        assert!(
            String::from_utf8_lossy(&out.stdout).is_empty(),
            "{key}: the session dies before the advertisement"
        );

        // A severity git accepts must be accepted, and must not be mistaken for
        // an unknown id.
        fx.ok(&bare, &["config", &key, "warn"]);
        let out = fx.stdin_run(&bare, &["receive-pack", &dir]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("Unknown fsck message type") && !stderr.contains("unknown msg id"),
            "{key}=warn: {stderr}"
        );
        assert!(
            !String::from_utf8_lossy(&out.stdout).is_empty(),
            "{key}=warn: the advertisement must still be written"
        );
        fx.ok(&bare, &["config", "--unset", &key]);
    }
}

/// `gitmodulesLarge` over a transfer: it needs the `index-pack` branch on both
/// sides, because that is the only reader that hands `fsck_blob()` a null buffer.
#[test]
fn gitmodules_large_over_a_transfer_needs_index_pack() {
    let fx = Fixture::new("gmlarge");
    let src = fx.big_gitmodules_source();

    // --- fetch. `fetch_pack()` picks `index-pack` whenever the check is on
    // (`fetch-pack.c:1007`), so the threshold alone decides.
    let dst = fx.empty("d-gm-off");
    let (stderr, code) = fetch(&fx, &dst, &src, &["-c", "core.bigFileThreshold=500"]);
    assert_eq!(code, 0, "{stderr}");

    let dst = fx.empty("d-gm-on");
    assert_eq!(
        fetch(
            &fx,
            &dst,
            &src,
            &["-c", "core.bigFileThreshold=500", "-c", "fetch.fsckObjects=true"]
        ),
        (
            format!(
                "error: object {BIG_BLOB}: {GITMODULES_LARGE}\n\
                 fatal: fsck error in packed object\n\
                 fatal: index-pack failed\n"
            ),
            128
        )
    );

    // Strictly greater, here too.
    let dst = fx.empty("d-gm-boundary");
    let (stderr, code) = fetch(
        &fx,
        &dst,
        &src,
        &["-c", "core.bigFileThreshold=501", "-c", "fetch.fsckObjects=true"],
    );
    assert_eq!(code, 0, "{stderr}");

    // `fetch.fsck.gitmodulesLarge` demotes it like any other id.
    let dst = fx.empty("d-gm-ignore");
    let (stderr, code) = fetch(
        &fx,
        &dst,
        &src,
        &[
            "-c",
            "core.bigFileThreshold=500",
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "fetch.fsck.gitmodulesLarge=ignore",
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("gitmodulesLarge"), "{stderr}");

    // --- push. `unpack()` picks `unpack-objects` for a pack under
    // `receive.unpackLimit` (`builtin/receive-pack.c:2369`), and that child never
    // fscks a blob per object, so the id cannot fire there.
    let dst = fx.bare("bare-gm-loose");
    fx.ok(&dst, &["config", "receive.fsckObjects", "true"]);
    fx.ok(&dst, &["config", "core.bigFileThreshold", "500"]);
    let out = fx.run(&src, &["push", dst.to_str().unwrap(), "main:refs/heads/main"]);
    assert!(
        out.status.success(),
        "unpack-objects has no per-object blob fsck: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // With `receive.unpackLimit=1` the same push goes through `index-pack`, and
    // the id fires — rejecting the ref.
    let dst = fx.bare("bare-gm-indexed");
    for (k, v) in [
        ("receive.fsckObjects", "true"),
        ("core.bigFileThreshold", "500"),
        ("receive.unpackLimit", "1"),
    ] {
        fx.ok(&dst, &["config", k, v]);
    }
    let out = fx.run(&src, &["push", dst.to_str().unwrap(), "main:refs/heads/main"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("remote: error: object {BIG_BLOB}: {GITMODULES_LARGE}")),
        "{stderr}"
    );
    assert!(stderr.contains("remote: fatal: fsck error in packed object"), "{stderr}");
    assert!(stderr.contains("error: remote unpack failed: index-pack abnormal exit"), "{stderr}");
    assert_eq!(fx.ok(&dst, &["for-each-ref"]), "", "the ref must not have moved");

    // `receive.fsck.gitmodulesLarge=ignore` lets the same push through, which is
    // the whole point of the family.
    let dst = fx.bare("bare-gm-ignore");
    for (k, v) in [
        ("receive.fsckObjects", "true"),
        ("core.bigFileThreshold", "500"),
        ("receive.unpackLimit", "1"),
        ("receive.fsck.gitmodulesLarge", "ignore"),
    ] {
        fx.ok(&dst, &["config", k, v]);
    }
    let out = fx.run(&src, &["push", dst.to_str().unwrap(), "main:refs/heads/main"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        fx.ok(&dst, &["for-each-ref"]).contains("refs/heads/main"),
        "the demoted push must land"
    );

    // `transfer.fsckObjects` is `receive.fsckObjects`' fallback here too.
    let dst = fx.bare("bare-gm-transfer");
    for (k, v) in [
        ("transfer.fsckObjects", "true"),
        ("core.bigFileThreshold", "500"),
        ("receive.unpackLimit", "1"),
    ] {
        fx.ok(&dst, &["config", k, v]);
    }
    let out = fx.run(&src, &["push", dst.to_str().unwrap(), "main:refs/heads/main"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(GITMODULES_LARGE),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
