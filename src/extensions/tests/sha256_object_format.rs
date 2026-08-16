//! The sha256 object format (`extensions.objectformat = sha256`), end to end.
//!
//! `git version --build-options` prints `SHA-256: SHA256_BLK`, which is a claim
//! that this build has a sha256 backend. A backend that only compiles is not a
//! backend, so what is asserted here is the behaviour behind the line: a sha256
//! repository has to be *the same repository stock git would have written*, and
//! the object ids in it have to be the ones stock git would have produced.
//!
//! Object ids are the strongest check available. An id is the hash function
//! applied to the exact bytes git writes for an object, so an id that matches
//! stock proves the algorithm, the object encoding and the storage layout all
//! agree at once — none of the three can be wrong on its own without moving it.
//!
//! Each test below covers a path where the hash width is *load-bearing*, i.e.
//! where a 40-hex or 20-byte assumption produces a wrong answer rather than a
//! compile error. Those are the paths that were actually broken:
//!
//!   * loose-object prefix lookup — `Kind::from_hex_len()` maps any prefix of 40
//!     characters or fewer to `Sha1`, so a `Prefix` built from a short spec
//!     carries a sha1-kind id. The loose store used that kind for the width of
//!     the file names it walks, and so matched nothing in a sha256 repository.
//!     Abbreviated revisions silently failed to resolve, which broke `rebase`
//!     (its todo list names commits by abbreviation).
//!   * `pack-objects --stdin` — `parse_oid_hex()` reads `the_hash_algo->hexsz`
//!     characters; reading a fixed 40 turned every 64-hex line into a sha1 id.
//!   * `receive-pack` — the pack a push delivers was indexed with a hardcoded
//!     sha1, so every push into a sha256 repository was rejected.
//!   * `bundle create` — a v2 bundle header carries no `@object-format`
//!     capability, so it can only describe sha1. Writing one for a sha256
//!     repository produced a bundle whose own reader split each 64-hex id at 40.
//!   * `fsck` — commit/tag headers and tree entries are sized in `hexsz`/`rawsz`.
//!
//! Every case is a differential one: the same commands run under stock git in a
//! parallel repository, and the two outputs compared. Without a stock git
//! installed there is nothing to compare against and the case is skipped rather
//! than weakened into a self-consistency check.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Where a real git lives. `git` on `PATH` is deliberately not consulted: this
/// binary shadows stock by name wherever zvcs is installed, so resolving it
/// there would drive this binary on both sides of the comparison.
const STOCK_CANDIDATES: [&str; 3] = ["/opt/homebrew/bin/git", "/usr/local/bin/git", "/usr/bin/git"];

/// The first candidate that exists and is not this binary wearing git's name.
///
/// The probe is a superset verb run with an emptied environment: zvcs serves
/// `zverbs` itself, while a stock git looks for a `git-zverbs` on `PATH` and
/// fails. Clearing the environment is what makes it sound — zvcs's installation
/// puts a `git-zverbs` shim on `PATH`, which a stock git would then answer too.
fn stock_git() -> Option<&'static str> {
    let scratch = std::env::temp_dir().join(format!("zvcs-s256probe-{}", std::process::id()));
    let found = STOCK_CANDIDATES.into_iter().find(|bin| {
        Path::new(bin).exists()
            && !Command::new(bin)
                .arg("zverbs")
                .env_clear()
                .env("ZVCS_HOME", &scratch)
                .current_dir(std::env::temp_dir())
                .output()
                .map(|o| o.status.success() && !o.stdout.is_empty())
                .unwrap_or(false)
    });
    let _ = std::fs::remove_dir_all(&scratch);
    found
}

/// A throwaway root holding one directory per binary under test.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-sha256-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

/// Run `bin` in `dir` with a pinned identity and clock, so a commit's id is a
/// function of its content alone and the two binaries can be compared on it.
fn git(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "100000000 +0000")
        .env("GIT_COMMITTER_DATE", "100000000 +0000")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = git(bin, dir, home, args);
    assert!(
        out.status.success(),
        "{bin} {args:?} in {dir:?} failed ({:?}): {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Build the same small sha256 history under both binaries and hand back
/// `(home, zvcs_dir, stock_dir)`. The history is deliberately shaped to exercise
/// the paths hash width matters on: a subdirectory (nested trees), two commits
/// (a `parent` header), and an annotated tag (an `object` header).
///
/// Returns `None` when there is no stock git to compare against, or when the one
/// installed has no sha256 support of its own.
fn twin_repos(tag: &str) -> Option<(&'static str, PathBuf, PathBuf, PathBuf)> {
    let stock = stock_git()?;
    let home = fixture(tag);
    let zvcs_dir = home.join("zvcs");
    let stock_dir = home.join("stock");
    std::fs::create_dir_all(&zvcs_dir).unwrap();
    std::fs::create_dir_all(&stock_dir).unwrap();

    let init = |bin: &str, at: &Path| {
        git(bin, at, &home, &["init", "-q", "--object-format=sha256", "-b", "main", "."])
    };
    if !init(stock, &stock_dir).status.success() {
        // This stock build has no sha256 backend; there is nothing to compare.
        return None;
    }
    assert!(init(BIN, &zvcs_dir).status.success(), "zvcs init --object-format=sha256 failed");

    for at in [&zvcs_dir, &stock_dir] {
        std::fs::write(at.join("f.txt"), "hello\n").unwrap();
        std::fs::create_dir_all(at.join("sub")).unwrap();
        std::fs::write(at.join("sub/g.txt"), "nested\n").unwrap();
    }
    for (bin, at) in [(BIN, &zvcs_dir), (stock, &stock_dir)] {
        ok(bin, at, &home, &["add", "-A"]);
        ok(bin, at, &home, &["commit", "-q", "-m", "one"]);
        std::fs::write(at.join("f.txt"), "hello\nmore\n").unwrap();
        ok(bin, at, &home, &["commit", "-q", "-am", "two"]);
        ok(bin, at, &home, &["tag", "-a", "v1", "-m", "tagmsg"]);
    }
    Some((stock, home, zvcs_dir, stock_dir))
}

/// `git init --object-format=sha256` must write the repository stock writes:
/// the `extensions.objectformat` key *and* the `core.repositoryformatversion`
/// bump every extension requires. Getting only one of the two produces a
/// repository stock git refuses to open.
#[test]
fn init_records_the_object_format_the_way_stock_does() {
    let Some((_stock, _home, zvcs_dir, stock_dir)) = twin_repos("init") else {
        eprintln!("skipping: no stock git with sha256 support to compare against");
        return;
    };

    let config = std::fs::read_to_string(zvcs_dir.join(".git/config")).unwrap();
    assert_eq!(
        config,
        std::fs::read_to_string(stock_dir.join(".git/config")).unwrap(),
        "sha256 init wrote a different config than stock"
    );
    assert!(config.contains("objectformat = sha256"), "{config}");
    assert!(config.contains("repositoryformatversion = 1"), "{config}");
}

/// Every id the two histories produced must agree, and each must be a full
/// sha256 digest rather than a sha1 one that happened to be padded.
#[test]
fn the_object_ids_are_stocks_object_ids() {
    let Some((stock, home, zvcs_dir, stock_dir)) = twin_repos("ids") else {
        eprintln!("skipping: no stock git with sha256 support to compare against");
        return;
    };

    let ids = |bin: &str, at: &Path| {
        // Every object reachable from every ref, with its type — blobs, both
        // trees, both commits and the tag object.
        ok(bin, at, &home, &["rev-list", "--objects", "--all"])
            .lines()
            .map(|l| l.split_whitespace().next().unwrap().to_string())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let zvcs_ids = ids(BIN, &zvcs_dir);
    assert_eq!(zvcs_ids, ids(stock, &stock_dir), "reachable object ids differ from stock");
    assert!(!zvcs_ids.is_empty(), "no objects were walked");
    for id in &zvcs_ids {
        assert_eq!(id.len(), 64, "{id} is not a sha256 object id");
    }

    // The ids the ref layer reports, which read through a different path than
    // the object walk above.
    let refs = |bin: &str, at: &Path| {
        ok(bin, at, &home, &["for-each-ref", "--format=%(refname) %(objectname) %(objecttype)"])
    };
    assert_eq!(refs(BIN, &zvcs_dir), refs(stock, &stock_dir));
}

/// A prefix shorter than 41 characters is what `Kind::from_hex_len()` calls a
/// sha1, so an abbreviated revision is the case where a sha256 repository is
/// most easily mistaken for a sha1 one. Every prefix length from git's minimum
/// up to the full digest must resolve to the same id, against *loose* objects —
/// packing them takes a different lookup path that was never broken, so a
/// packed-only test would pass while the bug was live.
#[test]
fn abbreviated_revisions_resolve_against_loose_objects() {
    let Some((stock, home, zvcs_dir, stock_dir)) = twin_repos("abbrev") else {
        eprintln!("skipping: no stock git with sha256 support to compare against");
        return;
    };
    // Nothing has been packed, so `.git/objects/??/…` is the only place to look.
    assert!(
        !zvcs_dir.join(".git/objects/pack").read_dir().unwrap().any(|e| e.is_ok()),
        "the fixture is packed, so this would not exercise the loose lookup"
    );

    let full = ok(BIN, &zvcs_dir, &home, &["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(full.len(), 64);
    assert_eq!(full, ok(stock, &stock_dir, &home, &["rev-parse", "HEAD"]).trim());

    for len in [4, 7, 8, 12, 20, 40, 41, 64] {
        let prefix = &full[..len];
        let got = ok(BIN, &zvcs_dir, &home, &["rev-parse", prefix]);
        assert_eq!(got.trim(), full, "`rev-parse {prefix}` ({len} chars) did not resolve");
    }

    // The abbreviation this build *writes* has to round-trip through the reader
    // that consumes it — this is the pairing `rebase`'s todo list relies on.
    let short = ok(BIN, &zvcs_dir, &home, &["rev-parse", "--short", "HEAD"]).trim().to_string();
    assert_eq!(short, ok(stock, &stock_dir, &home, &["rev-parse", "--short", "HEAD"]).trim());
    assert_eq!(ok(BIN, &zvcs_dir, &home, &["rev-parse", &short]).trim(), full);
}

/// `pack-objects --stdin` reads ids as `parse_oid_hex()` does, at the
/// repository's hex width. The pack it writes is then indexed and read back, so
/// a wrong width cannot hide: a truncated id names no object and the pack comes
/// out empty or unreadable.
#[test]
fn pack_objects_reads_and_writes_sha256_ids() {
    let Some((stock, home, zvcs_dir, stock_dir)) = twin_repos("pack") else {
        eprintln!("skipping: no stock git with sha256 support to compare against");
        return;
    };

    // Feed the object list to `pack-objects` on stdin — the path that reads ids
    // with `parse_oid_hex()` — and hand back every id the resulting index lists.
    let pack_and_list = |bin: &str, at: &Path| -> Vec<String> {
        use std::io::Write;

        let listing = ok(bin, at, &home, &["rev-list", "--objects", "--all"]);
        let ids: String = listing
            .lines()
            .map(|l| format!("{}\n", l.split_whitespace().next().unwrap()))
            .collect();

        let spawn = |args: &[&str], stdin_bytes: &[u8]| -> Output {
            let mut child = Command::new(bin)
                .args(args)
                .current_dir(at)
                .env("HOME", &home)
                .env("ZVCS_HOME", &home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("LC_ALL", "C")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(stdin_bytes).unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "{bin} {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };

        let out = spawn(&["pack-objects", "--quiet", "out"], ids.as_bytes());
        let name = String::from_utf8(out.stdout).unwrap().trim().to_string();
        assert_eq!(name.len(), 64, "pack name {name} is not a sha256 digest");

        // `show-index` reads the index on stdin. Its per-object lines are
        // `<offset> <oid>[ (<crc>)]`, so the id is the second field.
        let idx = at.join(format!("out-{name}.idx"));
        assert!(idx.is_file(), "no index beside the pack at {idx:?}");
        let out = spawn(&["show-index", "--object-format=sha256"], &std::fs::read(&idx).unwrap());
        let mut listed: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
            .collect();
        listed.sort();
        assert!(!listed.is_empty(), "the pack index listed no objects");
        for id in &listed {
            assert_eq!(id.len(), 64, "the index lists {id}, which is not a sha256 id");
        }
        listed
    };

    assert_eq!(
        pack_and_list(BIN, &zvcs_dir),
        pack_and_list(stock, &stock_dir),
        "the two packs index different objects"
    );
}

/// A push delivers a pack the receiving side has to index in its own hash. With
/// a hardcoded sha1 the index refuses the pack and every ref is rejected, so
/// this asserts the refs actually landed rather than just that the command
/// exited zero — `push` reports failure per ref.
#[test]
fn push_into_a_sha256_repository_lands_its_refs() {
    let Some((stock, home, zvcs_dir, stock_dir)) = twin_repos("push") else {
        eprintln!("skipping: no stock git with sha256 support to compare against");
        return;
    };

    let push_and_read = |bin: &str, at: &Path, bare: &Path| -> String {
        assert!(
            git(bin, at, &home, &["init", "-q", "--bare", "--object-format=sha256", bare.to_str().unwrap()])
                .status
                .success(),
            "bare sha256 init failed"
        );
        let out = git(bin, at, &home, &["push", bare.to_str().unwrap(), "--all"]);
        assert!(
            out.status.success(),
            "{bin} push failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The receiving repository is asked what it now holds, and is asked to
        // walk it: a pack indexed under the wrong hash fails here even if the
        // refs were somehow written.
        let refs = ok(bin, bare, &home, &["for-each-ref", "--format=%(refname) %(objectname)"]);
        let fsck = git(bin, bare, &home, &["fsck"]);
        assert!(fsck.status.success(), "fsck on the pushed-to repository failed: {fsck:?}");
        refs
    };

    let zvcs_refs = push_and_read(BIN, &zvcs_dir, &home.join("zvcs-bare"));
    assert!(zvcs_refs.contains("refs/heads/main"), "main did not land:\n{zvcs_refs}");
    assert_eq!(
        zvcs_refs,
        push_and_read(stock, &stock_dir, &home.join("stock-bare")),
        "the pushed refs differ from stock"
    );
}

/// A v2 bundle header has no `@object-format` capability, so it can only
/// describe sha1: git defaults a sha256 repository to v3 and refuses an explicit
/// `--version=2`. Writing a v2 bundle anyway produces a file whose own reader
/// splits each 64-hex id at 40, which is a silently corrupt bundle rather than
/// an error — so both halves are asserted.
#[test]
fn bundles_from_a_sha256_repository_are_version_3() {
    let Some((stock, home, zvcs_dir, stock_dir)) = twin_repos("bundle") else {
        eprintln!("skipping: no stock git with sha256 support to compare against");
        return;
    };

    let create = |bin: &str, at: &Path, to: &Path| {
        ok(bin, at, &home, &["bundle", "create", to.to_str().unwrap(), "--all"]);
        std::fs::read(to).unwrap()
    };
    let zvcs_bundle = create(BIN, &zvcs_dir, &home.join("zvcs.bundle"));
    let stock_bundle = create(stock, &stock_dir, &home.join("stock.bundle"));

    let header = |b: &[u8]| String::from_utf8_lossy(&b[..b.len().min(64)]).to_string();
    assert!(
        header(&zvcs_bundle).starts_with("# v3 git bundle\n@object-format=sha256\n"),
        "a sha256 bundle must default to v3 with the object-format capability:\n{}",
        header(&zvcs_bundle)
    );
    assert_eq!(header(&zvcs_bundle), header(&stock_bundle));

    // The bundle has to read back — this is where a 40-hex split would show up,
    // as a ref listing with a mangled id.
    let listed = |bin: &str, at: &Path, b: &Path| {
        ok(bin, at, &home, &["bundle", "list-heads", b.to_str().unwrap()])
    };
    let zvcs_heads = listed(BIN, &zvcs_dir, &home.join("zvcs.bundle"));
    for line in zvcs_heads.lines() {
        let id = line.split_whitespace().next().unwrap();
        assert_eq!(id.len(), 64, "bundle head {id} is not a whole sha256 id");
    }
    assert_eq!(zvcs_heads, listed(stock, &stock_dir, &home.join("stock.bundle")));

    // `--version=2` cannot describe this repository, and git refuses rather than
    // writing a bundle that would be misread.
    let refused = git(
        BIN,
        &zvcs_dir,
        &home,
        &["bundle", "create", "--version=2", home.join("v2.bundle").to_str().unwrap(), "--all"],
    );
    assert!(!refused.status.success(), "--version=2 was accepted in a sha256 repository");
    assert_eq!(
        String::from_utf8_lossy(&refused.stderr),
        "fatal: cannot write bundle version 2 with algorithm sha256\n"
    );
}

/// `fsck` decodes commit headers, tag headers and tree entries by offset, all of
/// which are sized in the repository's hash. A sha1 assumption reports the whole
/// history as corrupt, so a clean walk of a history containing nested trees, a
/// `parent` header and a tag object is the assertion.
#[test]
fn fsck_walks_a_sha256_repository_clean() {
    let Some((stock, home, zvcs_dir, stock_dir)) = twin_repos("fsck") else {
        eprintln!("skipping: no stock git with sha256 support to compare against");
        return;
    };

    for strict in [&["fsck"][..], &["fsck", "--strict"][..]] {
        let out = git(BIN, &zvcs_dir, &home, strict);
        let stock_out = git(stock, &stock_dir, &home, strict);
        assert_eq!(
            out.status.success(),
            stock_out.status.success(),
            "{strict:?} disagreed with stock on exit status"
        );
        assert!(
            out.stdout.is_empty() && out.stderr.is_empty(),
            "{strict:?} reported problems in a sound sha256 repository: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
