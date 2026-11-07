//! `pack-objects --delta-islands` and `repack -i`, which port `delta-islands.c`.
//!
//! * the internal rev list walks under `--topo-order` when islands are on
//!   (builtin/pack-objects.c:5440-5441), so the pack's entry order is the
//!   topological walk's — visible here through a fork committed with a date
//!   older than its parent's;
//! * `load_delta_islands()` names each ref's island from the capture groups of
//!   the last `pack.island` regex it matches, collapses islands whose tips sum
//!   to the same hash, and marks every tip, peeling tags
//!   (delta-islands.c:190-224, 393-500); `show_commit()` and
//!   `resolve_tree_islands()` carry the marks down (:239-310, :502-516);
//! * `island_delta_cmp()` and `in_same_island()` steer the delta search, and
//!   `pack.islandCore` splits the write order into two layers
//!   (`compute_pack_layers()`, :534-556) — bit 0's island when no island has
//!   that name;
//! * a bad regex is `failed to load island regex`, a valueless key is
//!   `config_error_nonbool()`, a regex with fifteen groups warns, `--progress`
//!   prints `Marked <n> islands, done.` and `Propagating island marks`, and a
//!   bare object list never loads islands at all (builtin/pack-objects.c:4955-4956).
//!
//! The delta search runs on one thread on both sides: stock's work stealing
//! between threads is not modelled, which is a separate matter.
//!
//! Stock git is the oracle (`support/stock_git.rs`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// Run `bin` in `dir` with `stdin`, the committer and author pinned to `date`.
fn git_at(bin: &str, dir: &Path, args: &[&str], stdin: &[u8], date: u64) -> (i32, Vec<u8>, Vec<u8>) {
    use std::io::Write;
    let date = format!("{date} +0000");
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.status.code().expect("no signal"), out.stdout, out.stderr)
}

fn git(bin: &str, dir: &Path, args: &[&str], stdin: &[u8]) -> (i32, Vec<u8>, Vec<u8>) {
    git_at(bin, dir, args, stdin, 1_700_000_000)
}

struct World {
    root: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl World {
    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }
}

/// A repository of forks: `main`, and `fork-one` .. `fork-four` off it, three
/// commits each, all appending to the same nested paths so that their blobs
/// are delta candidates for each other. The forks are also published under
/// `refs/remotes/<fork>/`, `two` twice. `fork-two` is committed with dates
/// older than its parent's, so a date-ordered walk and a topological one
/// disagree. `v1` is an annotated tag of a tag on `fork-one`, `lt` a
/// lightweight tag inside `fork-two`.
fn world(stock: &str, label: &str) -> World {
    let root = std::env::temp_dir().join(format!("zvcs-delta-islands-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let w = World { root };
    let repo = w.repo();
    std::fs::create_dir_all(repo.join("src/sub")).unwrap();
    let run = |args: &[&str], date: u64| {
        let out = git_at(stock, &repo, args, b"", date);
        assert_eq!(out.0, 0, "{args:?}: {}", String::from_utf8_lossy(&out.2));
    };
    let append = |path: &str, line: &str| {
        let mut text = std::fs::read_to_string(repo.join(path)).unwrap();
        text.push_str(line);
        std::fs::write(repo.join(path), text).unwrap();
    };
    // A commit runs `maintenance run --auto` in the background on stock;
    // switched off so nothing writes into `objects/` behind the test's back.
    let commit = |message: &str, date: u64| {
        run(&["add", "."], date);
        run(&["-c", "maintenance.auto=false", "commit", "-q", "-m", message], date);
    };
    const T: u64 = 1_700_000_000;
    run(&["init", "-q", "-b", "main"], T);
    let a: String = (1..=60).map(|i| format!("line {i} common content for the file here\n")).collect();
    let b: String = (1..=40).map(|i| format!("sub {i} deeper content\n")).collect();
    std::fs::write(repo.join("src/a.txt"), &a).unwrap();
    std::fs::write(repo.join("src/sub/b.txt"), &b).unwrap();
    std::fs::write(repo.join("top.txt"), &a).unwrap();
    commit("base", T);
    let mut t = T + 100;
    for fork in ["one", "two", "three", "four"] {
        run(&["checkout", "-q", "-b", &format!("fork-{fork}"), "main"], T);
        for k in 1..=3u64 {
            append("src/a.txt", &format!("{fork} change {k}\n"));
            append("src/sub/b.txt", &format!("{fork} sub {k}\n"));
            append("top.txt", &format!("{fork} top {k}\n"));
            if fork == "two" {
                commit(&format!("{fork} {k}"), 1_600_000_000 + k);
            } else {
                commit(&format!("{fork} {k}"), t);
                t += 100;
            }
        }
    }
    run(&["checkout", "-q", "main"], T);
    append("src/a.txt", "main extra\n");
    commit("main 2", t);
    for (name, target) in [
        ("refs/remotes/one/main", "fork-one"),
        ("refs/remotes/two/main", "fork-two"),
        ("refs/remotes/two/dev", "fork-two~1"),
        ("refs/remotes/three/main", "fork-three"),
    ] {
        run(&["update-ref", name, target], T);
    }
    run(&["tag", "-a", "-m", "inner", "inner", "fork-one"], t);
    run(&["tag", "-a", "-m", "outer", "v1", "inner"], t);
    run(&["tag", "-d", "inner"], T);
    run(&["tag", "lt", "fork-two~2"], T);
    w
}

/// `pack-objects <args>` fed `stdin` in a fresh pair of worlds, each configured
/// with `config` first; exit, stdout and stderr must agree byte for byte.
#[track_caller]
fn same_pack(label: &str, config: &[(&str, &str)], args: &[&str], stdin: &[u8]) -> (i32, Vec<u8>, Vec<u8>) {
    let Some(stock) = stock_git() else { return (0, Vec::new(), Vec::new()) };
    let s = world(stock, &format!("{label}-stock"));
    let z = world(stock, &format!("{label}-zvcs"));
    for w in [&s, &z] {
        for (key, value) in config {
            let out = git(stock, &w.repo(), &["config", "--add", key, value], b"");
            assert_eq!(out.0, 0, "config {key}");
        }
    }
    let mut full = vec!["pack-objects"];
    full.extend_from_slice(args);
    let want = git(stock, &s.repo(), &full, stdin);
    let got = git(ZVCS, &z.repo(), &full, stdin);
    assert_eq!(
        String::from_utf8_lossy(&got.2),
        String::from_utf8_lossy(&want.2),
        "{label}: stderr"
    );
    assert_eq!(got.0, want.0, "{label}: exit");
    assert!(got.1 == want.1, "{label}: the packs differ ({} vs {} bytes)", got.1.len(), want.1.len());
    want
}

const ISLAND_PACK: &[&str] = &["--delta-islands", "--all", "--stdout", "--threads=1", "-q"];

#[test]
fn one_island_per_remote() {
    let (code, pack, _) = same_pack("remotes", &[("pack.island", "refs/remotes/([^/]+)/")], ISLAND_PACK, b"");
    assert_eq!(code, 0);
    assert!(pack.starts_with(b"PACK"));
}

#[test]
fn one_island_per_branch_with_a_core() {
    same_pack(
        "heads-core",
        &[("pack.island", "refs/heads/(.*)"), ("pack.islandCore", "main")],
        ISLAND_PACK,
        b"",
    );
}

/// Two regexes, the later one winning for the refs both match; groups that
/// take no part are skipped in the name; and a core naming no island falls
/// back to bit 0, whichever island the khash iteration put first.
#[test]
fn last_regex_wins_and_a_missing_core_is_bit_zero() {
    same_pack(
        "multi",
        &[
            ("pack.island", "refs/(heads|remotes)/([^/]*)"),
            ("pack.island", "refs/(tags)/(x)?"),
            ("pack.islandCore", "nonexistent"),
        ],
        ISLAND_PACK,
        b"",
    );
}

/// Under `--revs` the walk is still the internal rev list, so islands load and
/// the topological order applies to what the revisions reach.
#[test]
fn revs_on_stdin() {
    same_pack(
        "revs",
        &[("pack.island", "refs/remotes/([^/]+)/")],
        &["--delta-islands", "--revs", "--stdout", "--threads=1", "-q"],
        b"fork-one\nfork-two\n^fork-three\n",
    );
}

#[test]
fn diagnostics() {
    same_pack("bad-regex", &[("pack.island", "refs/(")], ISLAND_PACK, b"");
    same_pack(
        "many-groups",
        &[("pack.island", "r(e)(f)(s)(/)(h)(e)(a)(d)(s)(/)(m)(a)(i)(n)(.*)")],
        ISLAND_PACK,
        b"",
    );
    same_pack(
        "progress",
        &[("pack.island", "refs/heads/(.*)")],
        &["--delta-islands", "--all", "--stdout", "--threads=1", "--progress"],
        b"",
    );
    let Some(stock) = stock_git() else { return };
    // `git config --add pack.island` cannot write a valueless key; `-c` can.
    for (label, key) in [("nonbool-island", "pack.island"), ("nonbool-core", "pack.islandCore")] {
        let s = world(stock, &format!("{label}-stock"));
        let z = world(stock, &format!("{label}-zvcs"));
        let args = ["-c", key, "pack-objects", "--delta-islands", "--all", "--stdout", "-q"];
        let want = git(stock, &s.repo(), &args, b"");
        let got = git(ZVCS, &z.repo(), &args, b"");
        assert_eq!(String::from_utf8_lossy(&got.2), String::from_utf8_lossy(&want.2), "{label}");
        assert_eq!((got.0, got.1.is_empty()), (want.0, want.1.is_empty()), "{label}");
    }
}

/// A bare object list never reaches `get_object_list()`, so the islands are
/// never loaded and even a regex that would not compile goes unread.
#[test]
fn a_bare_object_list_has_no_islands() {
    let Some(stock) = stock_git() else { return };
    let w = world(stock, "bare-list-ids");
    // With the names `rev-list --objects` prints, which `add_object_entry()`
    // hashes for the delta search.
    let ids = git(stock, &w.repo(), &["rev-list", "--objects", "--all"], b"").1;
    let (code, _, err) = same_pack(
        "bare-list",
        &[("pack.island", "refs/(")],
        &["--delta-islands", "--stdout", "--threads=1", "-q"],
        &ids,
    );
    assert_eq!((code, err.is_empty()), (0, true));
}

/// `repack -a -d -i` and `repack.useDeltaIslands`: the same pack, by name and
/// by `verify-pack -v`, from loose objects and again from an existing pack
/// whose deltas the islands must vet before reusing.
#[test]
fn repack_with_islands() {
    let Some(stock) = stock_git() else { return };
    let cases: &[(&str, &[(&str, &str)], &[&str], bool)] = &[
        ("repack-remotes", &[("pack.island", "refs/remotes/([^/]+)/")], &["-a", "-d", "-i", "-q"], false),
        (
            "repack-core",
            &[("pack.island", "refs/heads/(.*)"), ("pack.islandCore", "main")],
            &["-a", "-d", "-i", "-q"],
            false,
        ),
        (
            "repack-config",
            &[("repack.useDeltaIslands", "true"), ("pack.island", "refs/tags/(.*)")],
            &["-a", "-d", "-q"],
            false,
        ),
        ("repack-reuse", &[("pack.island", "refs/remotes/([^/]+)/")], &["-a", "-d", "-i", "-q"], true),
    ];
    for (label, config, args, prepacked) in cases {
        let s = world(stock, &format!("{label}-stock"));
        let z = world(stock, &format!("{label}-zvcs"));
        let mut listings = Vec::new();
        for (bin, w) in [(stock, &s), (ZVCS, &z)] {
            let repo = w.repo();
            // `repack` packs `--indexed-objects` too; the index is moved aside
            // so the comparison is about the islands alone.
            std::fs::rename(repo.join(".git/index"), w.root.join("index")).unwrap();
            let mut setup: Vec<(&str, &str)> = vec![("pack.threads", "1")];
            setup.extend_from_slice(config);
            if *prepacked {
                assert_eq!(git(stock, &repo, &["-c", "pack.threads=1", "repack", "-a", "-d", "-q"], b"").0, 0);
            }
            for (key, value) in setup {
                assert_eq!(git(stock, &repo, &["config", "--add", key, value], b"").0, 0);
            }
            let mut full = vec!["repack"];
            full.extend_from_slice(args);
            let out = git(bin, &repo, &full, b"");
            let pack_dir = repo.join(".git/objects/pack");
            let mut names: Vec<String> = std::fs::read_dir(&pack_dir)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            let verify: Vec<u8> = names
                .iter()
                .filter(|name| name.ends_with(".idx"))
                .flat_map(|name| git(stock, &pack_dir, &["verify-pack", "-v", name], b"").1)
                .collect();
            let fsck = git(stock, &repo, &["fsck"], b"").0;
            listings.push((out.0, String::from_utf8_lossy(&out.2).into_owned(), names, verify, fsck));
        }
        let (want, got) = (&listings[0], &listings[1]);
        assert_eq!((got.0, &got.1), (want.0, &want.1), "{label}: exit and stderr");
        assert_eq!(got.2, want.2, "{label}: objects/pack");
        assert!(got.3 == want.3, "{label}: verify-pack -v differs");
        assert_eq!((got.4, want.4), (0, 0), "{label}: fsck");
    }
}
