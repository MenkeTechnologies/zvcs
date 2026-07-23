//! `git commit-graph write` reads `commitGraph.generationVersion`, the one
//! commit-graph config key whose value changes the bytes this port writes. Value
//! 2 (git's default, and the effective value when the key is unset) records the
//! corrected commit date in the `GDA2` chunk (plus `GDO2` on overflow); any other
//! integer — 0, 1, 3, negatives — keeps only the topological level, which already
//! lives in the `CDAT` generation bits, and omits the corrected-date chunks. git
//! gates this on the value being *exactly* 2, and a non-numeric value is fatal.
//!
//! These tests pin the ported behavior byte-for-byte against stock git on an
//! octopus-merge history, so the `EDGE` chunk (used when a commit has more than
//! two parents) is present too and its ordering relative to the now-optional
//! `GDA2` is checked in both directions. There is no `--changed-paths` /
//! generation-number command-line flag that overrides this key, and this port
//! has no `-c` support, so the value is read from the repo's own `.git/config` —
//! exactly how both binaries are exercised here.
//!
//! The other two `commitGraph.*` keys are deliberately not tested: both only
//! tune the changed-path Bloom filters (`BIDX`/`BDAT`), which this port does not
//! produce (`--changed-paths` is rejected). `commitGraph.maxNewFilters` bounds
//! how many filters a write computes, and `commitGraph.readChangedPaths` gates
//! reading/reusing existing filters — neither surface exists here, so neither
//! key can change a byte of output.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// Commit with fixed author/committer dates so both binaries see identical
/// commit objects (hence identical graph input) across runs.
fn commit(repo: &Path, msg: &str, when: &str) {
    assert!(
        Command::new("git")
            .args(["commit", "-q", "-m", msg])
            .current_dir(repo)
            .env("GIT_COMMITTER_DATE", when)
            .env("GIT_AUTHOR_DATE", when)
            .status()
            .unwrap()
            .success(),
        "commit {msg:?} failed"
    );
}

/// A repo whose tip is an octopus merge with four parents, forcing the graph to
/// carry an `EDGE` chunk in addition to the version-gated `GDA2`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-cgcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);

    std::fs::write(repo.join("base"), "base\n").unwrap();
    git(&repo, &["add", "base"]);
    commit(&repo, "base", "@1700000000 +0000");

    for (i, br) in ["b1", "b2", "b3"].iter().enumerate() {
        git(&repo, &["checkout", "-q", "-b", br, "main"]);
        let name = format!("x{i}");
        std::fs::write(repo.join(&name), format!("{i}\n")).unwrap();
        git(&repo, &["add", &name]);
        commit(&repo, br, &format!("@{} +0000", 1_700_000_100 + i as i64));
    }

    git(&repo, &["checkout", "-q", "main"]);
    assert!(
        Command::new("git")
            .args(["merge", "-q", "--no-edit", "b1", "b2", "b3"])
            .current_dir(&repo)
            .env("GIT_COMMITTER_DATE", "@1700000200 +0000")
            .env("GIT_AUTHOR_DATE", "@1700000200 +0000")
            .status()
            .unwrap()
            .success(),
        "octopus merge failed"
    );

    (repo, home)
}

/// Set (or, with `None`, unset) `commitGraph.generationVersion` in the repo config.
fn set_generation_version(repo: &Path, value: Option<&str>) {
    match value {
        Some(v) => git(repo, &["config", "commitGraph.generationVersion", v]),
        None => {
            // `--unset` fails when the key is absent; ignore that.
            let _ = Command::new("git")
                .args(["config", "--unset", "commitGraph.generationVersion"])
                .current_dir(repo)
                .status();
        }
    }
}

/// Path to the single (non-split) commit-graph file.
fn graph_path(repo: &Path) -> PathBuf {
    repo.join(".git/objects/info/commit-graph")
}

/// Run `<bin> commit-graph write --reachable` under a hermetic environment and
/// return the bytes it wrote. The key is read from the repo's `.git/config`.
fn write_graph(bin: &str, repo: &Path, home: &Path) -> Vec<u8> {
    let _ = std::fs::remove_file(graph_path(repo));
    let out = Command::new(bin)
        .args(["commit-graph", "write", "--reachable"])
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{bin} commit-graph write failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read(graph_path(repo)).expect("commit-graph file was not written")
}

/// The ordered four-byte chunk ids of a commit-graph file, excluding the
/// terminating zero entry. Header layout: `CGPH`, version, hash version, chunk
/// count, base-graph count, then `(id, u64 offset)` lookup entries.
fn chunk_ids(bytes: &[u8]) -> Vec<String> {
    assert_eq!(&bytes[..4], b"CGPH", "bad signature");
    let n = bytes[6] as usize;
    (0..n)
        .map(|i| {
            let off = 8 + i * 12;
            String::from_utf8_lossy(&bytes[off..off + 4]).into_owned()
        })
        .collect()
}

/// Real git accepts the file this port wrote (self-consistent chunks + trailer).
fn real_git_verifies(repo: &Path, home: &Path) {
    let out = Command::new("git")
        .args(["commit-graph", "verify"])
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "real git rejected the zvcs commit-graph: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn default_writes_gda2_byte_for_byte() {
    let (repo, home) = fixture("default");
    set_generation_version(&repo, None);

    let zvcs = write_graph(BIN, &repo, &home);
    // The port's file is on disk now — let real git validate it before anything
    // overwrites it.
    real_git_verifies(&repo, &home);

    let git_bytes = write_graph("git", &repo, &home);
    assert_eq!(zvcs, git_bytes, "default write must be byte-identical to git");
    assert_eq!(
        chunk_ids(&zvcs),
        ["OIDF", "OIDL", "CDAT", "GDA2", "EDGE"],
        "default (v2) must carry GDA2 before EDGE"
    );
}

#[test]
fn generation_version_one_omits_gda2_byte_for_byte() {
    let (repo, home) = fixture("v1");
    set_generation_version(&repo, Some("1"));

    let zvcs = write_graph(BIN, &repo, &home);
    real_git_verifies(&repo, &home);

    let git_bytes = write_graph("git", &repo, &home);
    assert_eq!(zvcs, git_bytes, "v1 write must be byte-identical to git");
    assert_eq!(
        chunk_ids(&zvcs),
        ["OIDF", "OIDL", "CDAT", "EDGE"],
        "v1 must drop GDA2 while keeping EDGE last"
    );
}

#[test]
fn non_two_values_all_drop_gda2_and_differ_from_default() {
    let (repo, home) = fixture("nontwo");

    set_generation_version(&repo, None);
    let v2 = write_graph(BIN, &repo, &home);

    // 0, 1, 3 and a negative all mean "not 2" -> no corrected-date chunk, and
    // every one must equal git's output for the same value.
    for v in ["0", "1", "3", "-1"] {
        set_generation_version(&repo, Some(v));
        let zvcs = write_graph(BIN, &repo, &home);
        let git_bytes = write_graph("git", &repo, &home);
        assert_eq!(zvcs, git_bytes, "value {v} must be byte-identical to git");
        assert!(
            !chunk_ids(&zvcs).contains(&"GDA2".to_string()),
            "value {v} must omit GDA2"
        );
        assert_ne!(
            zvcs, v2,
            "value {v} must differ from the default (v2) graph — guards against an empty/no-op writer"
        );
    }
}

#[test]
fn invalid_generation_version_is_fatal_like_git() {
    let (repo, home) = fixture("invalid");

    for (value, reason) in [
        ("abc", "invalid unit"),
        ("2x", "invalid unit"),
        ("999999999999999999999", "out of range"),
    ] {
        set_generation_version(&repo, Some(value));
        let _ = std::fs::remove_file(graph_path(&repo));
        let out = Command::new(BIN)
            .args(["commit-graph", "write", "--reachable"])
            .current_dir(&repo)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("ZVCS_HOME", &home)
            .output()
            .unwrap();

        assert_eq!(
            out.status.code(),
            Some(128),
            "value {value:?} must be a fatal (128) error"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        let expected = format!(
            "fatal: bad numeric config value '{value}' for 'commitgraph.generationversion': {reason}\n"
        );
        assert_eq!(stderr, expected, "message mismatch for {value:?}");
        assert!(
            !graph_path(&repo).exists(),
            "no graph file may be written for the fatal value {value:?}"
        );
    }
}
