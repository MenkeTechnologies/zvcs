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
//! The remaining `commitGraph.*` keys all steer the changed-path Bloom filters
//! in the `BIDX`/`BDAT` chunks, and are covered further down:
//! `commitGraph.changedPaths` turns filters on without the command-line flag,
//! `commitGraph.changedPathsVersion` picks which murmur3 hashes them (and, at 0,
//! suppresses reading them at all), `commitGraph.readChangedPaths` is the
//! deprecated spelling of that last behavior, and `commitGraph.maxNewFilters`
//! bounds how many filters one write may compute. Each is pinned the same way as
//! the generation-version tests: byte-for-byte against stock git.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// Commit with fixed author/committer dates so both binaries see identical
/// commit objects (hence identical graph input) across runs.
fn commit(repo: &Path, msg: &str, when: &str) {
    assert!(
        Command::new(BIN)
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
        Command::new(BIN)
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
            let _ = Command::new(BIN)
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
    let out = Command::new(BIN)
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

// --- changed-path Bloom filters: BIDX/BDAT -------------------------------

/// The oldest stock git that can serve as an oracle here. `commitGraph.changedPaths`
/// and `commitGraph.changedPathsVersion` are not read by every git that ships as
/// `/usr/bin/git` — macOS 26 carries 2.50.1, which writes no filters no matter
/// what these keys say — so an older binary would "disagree" about behavior it
/// simply does not have.
const MIN_STOCK: (u32, u32) = (2, 55);

/// `(major, minor)` of a git binary, or `None` if it cannot be run or parsed.
fn git_version(path: &str) -> Option<(u32, u32)> {
    let out = Command::new(path).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let nums = text.split_whitespace().find(|w| w.starts_with(char::is_numeric))?;
    let mut it = nums.split('.');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// A stock git new enough to compare against, resolved explicitly rather than
/// through `PATH` — on a machine where zvcs shadows `git`, `PATH` resolution
/// would silently make the oracle the thing under test. When none is new enough
/// the byte-comparison half of a test is skipped and the zvcs-side assertions
/// still run.
fn stock_git() -> Option<String> {
    let candidates: Vec<String> = match std::env::var("ZVCS_STOCK_GIT") {
        Ok(p) => vec![p],
        Err(_) => ["/opt/homebrew/bin/git", "/usr/local/bin/git", "/usr/bin/git"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    };
    candidates
        .into_iter()
        .filter(|p| std::path::Path::new(p).exists())
        .find(|p| git_version(p).is_some_and(|v| v >= MIN_STOCK))
}

/// Delete the commit-graph, which both binaries leave read-only as git does.
fn remove_graph(repo: &Path) {
    let path = graph_path(repo);
    if path.exists() {
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(&path, perms);
        std::fs::remove_file(&path).unwrap();
    }
}

/// Put `bytes` back at the commit-graph path, replacing a read-only file.
fn restore_graph(repo: &Path, bytes: &[u8]) {
    remove_graph(repo);
    std::fs::write(graph_path(repo), bytes).unwrap();
}

/// `<bin> commit-graph write --reachable <args...>`, returning the bytes.
fn write_graph_with(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> Vec<u8> {
    remove_graph(repo);
    let out = Command::new(bin)
        .args(["commit-graph", "write", "--reachable"])
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{bin} commit-graph write {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read(graph_path(repo)).expect("commit-graph file was not written")
}

/// The `hash_version` field of the `BDAT` header, or `None` when the file
/// carries no filters. `BDAT` opens with three big-endian `u32`s, and the chunk
/// lookup gives its offset.
fn bdat_hash_version(bytes: &[u8]) -> Option<u32> {
    let n = bytes[6] as usize;
    let at = (0..n).find(|i| &bytes[8 + i * 12..8 + i * 12 + 4] == b"BDAT")?;
    let off = u64::from_be_bytes(
        bytes[8 + at * 12 + 4..8 + at * 12 + 12]
            .try_into()
            .expect("8 bytes"),
    ) as usize;
    Some(u32::from_be_bytes(
        bytes[off..off + 4].try_into().expect("4 bytes"),
    ))
}

/// Run the same sequence under zvcs and under stock git on two copies of the
/// same repo and require the resulting graphs to be identical. Returns the
/// bytes zvcs wrote so the caller can assert on them even without an oracle.
fn same_as_stock(tag: &str, config: &[(&str, &str)], args: &[&str]) -> Vec<u8> {
    let (repo, home) = fixture(tag);
    for (k, v) in config {
        git(&repo, &["config", k, v]);
    }
    let ours = write_graph_with(BIN, &repo, &home, args);

    if let Some(stock) = stock_git() {
        let theirs = write_graph_with(&stock, &repo, &home, args);
        assert_eq!(
            ours, theirs,
            "zvcs and stock git must write the same commit-graph for {config:?} {args:?}"
        );
        // And stock git must be able to read back what we wrote.
        restore_graph(&repo, &ours);
        let out = Command::new(&stock)
            .args(["commit-graph", "verify"])
            .current_dir(&repo)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stock git rejected the graph zvcs wrote: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    ours
}

#[test]
fn filters_are_absent_until_something_asks_for_them() {
    let bytes = same_as_stock("cp-off", &[], &[]);
    let ids = chunk_ids(&bytes);
    assert!(
        !ids.iter().any(|c| c == "BIDX") && !ids.iter().any(|c| c == "BDAT"),
        "a plain write carries no filters, got {ids:?}"
    );
}

#[test]
fn changed_paths_flag_writes_both_filter_chunks() {
    let bytes = same_as_stock("cp-flag", &[], &["--changed-paths"]);
    let ids = chunk_ids(&bytes);
    // The format requires the two to travel together, and git emits them last.
    assert_eq!(
        ids.iter().rev().take(2).rev().collect::<Vec<_>>(),
        vec!["BIDX", "BDAT"],
        "filters are written as a trailing BIDX/BDAT pair, got {ids:?}"
    );
    assert_eq!(bdat_hash_version(&bytes), Some(1), "version 1 is the default");
}

#[test]
fn changed_paths_config_turns_filters_on_without_the_flag() {
    let bytes = same_as_stock("cp-cfg", &[("commitGraph.changedPaths", "true")], &[]);
    assert!(
        chunk_ids(&bytes).iter().any(|c| c == "BDAT"),
        "commitGraph.changedPaths=true writes filters with no command-line flag"
    );
}

#[test]
fn changed_paths_version_two_selects_the_other_murmur3() {
    let v1 = same_as_stock(
        "cp-v1",
        &[("commitGraph.changedPathsVersion", "1")],
        &["--changed-paths"],
    );
    let v2 = same_as_stock(
        "cp-v2",
        &[("commitGraph.changedPathsVersion", "2")],
        &["--changed-paths"],
    );
    assert_eq!(bdat_hash_version(&v1), Some(1));
    assert_eq!(bdat_hash_version(&v2), Some(2), "the header records the version");
    assert_eq!(
        v1.len(),
        v2.len(),
        "the two versions hash the same paths, so the filters are the same size"
    );
}

#[test]
fn an_unsupported_changed_paths_version_writes_nothing() {
    let (repo, home) = fixture("cp-bad");
    git(&repo, &["config", "commitGraph.changedPathsVersion", "99"]);
    let _ = std::fs::remove_file(graph_path(&repo));
    let out = Command::new(BIN)
        .args(["commit-graph", "write", "--reachable", "--changed-paths"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    assert!(out.status.success(), "git warns rather than failing");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not supported"),
        "the warning names the unsupported version, got {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !graph_path(&repo).exists(),
        "an unsupported version aborts the whole write, not just the filters"
    );
}

#[test]
fn a_rewrite_keeps_filters_the_previous_graph_had() {
    let (repo, home) = fixture("cp-inherit");
    let with = write_graph_with(BIN, &repo, &home, &["--changed-paths"]);
    assert!(chunk_ids(&with).iter().any(|c| c == "BDAT"));

    // A plain rewrite must not silently drop them...
    let out = Command::new(BIN)
        .args(["commit-graph", "write", "--reachable"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    assert!(out.status.success());
    let again = std::fs::read(graph_path(&repo)).unwrap();
    assert_eq!(again, with, "an unqualified rewrite reproduces the filters");

    // ...but an explicit --no-changed-paths must.
    let without = write_graph_with(BIN, &repo, &home, &["--no-changed-paths"]);
    assert!(
        !chunk_ids(&without).iter().any(|c| c == "BDAT"),
        "--no-changed-paths drops the filters it inherited"
    );
}

#[test]
fn read_changed_paths_false_stops_filters_being_carried_over() {
    let (repo, home) = fixture("cp-read");
    let with = write_graph_with(BIN, &repo, &home, &["--changed-paths"]);
    assert!(chunk_ids(&with).iter().any(|c| c == "BDAT"));

    // The deprecated key is `changedPathsVersion = 0`, which is "read no
    // filters" — so there is nothing to inherit and the rewrite drops them.
    git(&repo, &["config", "commitGraph.readChangedPaths", "false"]);
    restore_graph(&repo, &with);
    let out = Command::new(BIN)
        .args(["commit-graph", "write", "--reachable"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .output()
        .unwrap();
    assert!(out.status.success());
    let after = std::fs::read(graph_path(&repo)).unwrap();
    assert!(
        !chunk_ids(&after).iter().any(|c| c == "BDAT"),
        "filters this run may not read are filters it cannot pass on"
    );
}

#[test]
fn max_new_filters_bounds_what_one_write_computes() {
    // Two of the five commits get a real filter; the rest are stored with a
    // length of zero, so the chunk is strictly smaller than an unbounded write.
    let bounded = same_as_stock("cp-max", &[], &["--changed-paths", "--max-new-filters", "2"]);
    let unbounded = same_as_stock("cp-nomax", &[], &["--changed-paths"]);
    assert!(
        bounded.len() < unbounded.len(),
        "a bounded write computes fewer filters, so BDAT is shorter: {} vs {}",
        bounded.len(),
        unbounded.len()
    );
    assert!(
        chunk_ids(&bounded).iter().any(|c| c == "BDAT"),
        "the chunks are still written, just with empty entries"
    );
}

#[test]
fn max_new_filters_config_matches_the_command_line_option() {
    let by_config = same_as_stock("cp-maxcfg", &[("commitGraph.maxNewFilters", "2")], &["--changed-paths"]);
    let by_flag = same_as_stock("cp-maxflag", &[], &["--changed-paths", "--max-new-filters", "2"]);
    assert_eq!(
        by_config, by_flag,
        "commitGraph.maxNewFilters is the default for --max-new-filters"
    );
}
