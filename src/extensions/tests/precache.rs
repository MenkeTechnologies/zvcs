//! Precomputed log caches must be an optimization and nothing else: whatever a
//! warmed cache makes fast, it has to render exactly what the cold path would.
//!
//! The daemon fills these caches on ref-change and `git zprecache` fills them on
//! demand, so a wrong entry would be served silently to every later command —
//! which is why the check here is byte equality of cold against warm output, not
//! merely that the rows exist.
//!
//! Each test uses its own `ZVCS_HOME`, so the developer's ledger is never read
//! or written.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn zvcs(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run zvcs git")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository with `n` commits that add, modify and delete files, so the
/// warmed change lists cover every status the stat formats can print.
fn fixture(tag: &str, n: usize) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-precache-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    std::fs::create_dir_all(&home).expect("mkdir home");
    assert!(zvcs(&repo, &home, &["init", "-q", "-b", "main"]).status.success(), "init");

    for i in 0..n {
        let keep = repo.join("keep.txt");
        let body: String = (0..=i).map(|k| format!("line {k}\n")).collect();
        std::fs::write(&keep, body).expect("write keep");
        let churn = repo.join(format!("churn{}.txt", i % 3));
        std::fs::write(&churn, format!("churn {i}\n")).expect("write churn");
        assert!(zvcs(&repo, &home, &["add", "-A"]).status.success(), "add");
        // Every third commit also deletes a file, so deletions are covered.
        if i % 3 == 2 {
            let doomed = repo.join(format!("churn{}.txt", (i + 1) % 3));
            if doomed.exists() {
                std::fs::remove_file(&doomed).expect("remove churn");
                assert!(zvcs(&repo, &home, &["add", "-A"]).status.success(), "add rm");
            }
        }
        let out = zvcs(&repo, &home, &["commit", "-q", "-m", &format!("commit {i}")]);
        assert!(out.status.success(), "commit {i}: {}", String::from_utf8_lossy(&out.stderr));
    }
    (repo, home)
}

/// A warmed ledger must not change a single byte of what `log` prints. Cold
/// output is captured first, the ledger is warmed, and the two are compared for
/// every format the cache feeds.
#[test]
fn warmed_caches_render_exactly_what_the_cold_path_does() {
    let (repo, home) = fixture("faithful", 12);
    let formats: [&[&str]; 5] = [
        &["log", "--stat"],
        &["log", "--numstat"],
        &["log", "--shortstat"],
        &["log", "--name-status"],
        &["log", "--oneline"],
    ];

    let cold: Vec<String> =
        formats.iter().map(|args| stdout_of(&zvcs(&repo, &home, args))).collect();
    assert!(cold.iter().all(|o| !o.is_empty()), "the fixture must produce output");

    let warm = zvcs(&repo, &home, &["zprecache", "-n", "50"]);
    assert!(warm.status.success(), "zprecache: {}", String::from_utf8_lossy(&warm.stderr));
    assert_eq!(stdout_of(&warm).trim(), "12", "every commit is warmed and reported");

    for (args, before) in formats.iter().zip(cold) {
        let after = stdout_of(&zvcs(&repo, &home, args));
        assert_eq!(after, before, "`{}` changed once the cache was warm", args.join(" "));
    }
}

/// Cache rows are written off the command's critical path, which is only safe
/// if the process still waits for them before exiting — a detached writer would
/// be killed at exit and the cache would never fill, turning every run into a
/// cold one while looking fine.
///
/// So: one cold run, then assert the entries are on disk when the process is
/// gone.
#[test]
fn a_single_run_leaves_its_cache_entries_on_disk() {
    let (repo, home) = fixture("async", 6);
    let cache = home.join("cache");
    let _ = std::fs::remove_dir_all(&cache);

    let out = zvcs(&repo, &home, &["log", "--stat"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // Either half of the store counts: a batch this small stays in the journal,
    // and a larger one would have been folded into the base image.
    let written = |name: &str| {
        ["rkyv", "log"]
            .iter()
            .filter_map(|ext| std::fs::metadata(cache.join(format!("{name}.{ext}"))).ok())
            .map(|m| m.len())
            .sum::<u64>()
    };
    assert!(written("treediff") > 0, "the run left its tree diffs on disk");
    // The default format prints full ids, so nothing abbreviates until a format
    // asks for a short one — that path has its own cache and its own writer.
    assert_eq!(written("abbrev"), 0, "no format asked for a short id yet");
    let short = zvcs(&repo, &home, &["log", "--oneline"]);
    assert!(short.status.success(), "{}", String::from_utf8_lossy(&short.stderr));
    assert!(written("abbrev") > 0, "the run left its abbreviations on disk");

    // Read them back with a fresh process, so nothing of the writer is still
    // around to make this pass.
    let counted = zvcs(&repo, &home, &["zprecache", "-n", "0"]);
    assert!(counted.status.success());

    // The second run must render exactly what the first did, now from the cache.
    let again = zvcs(&repo, &home, &["log", "--stat"]);
    assert_eq!(stdout_of(&again), stdout_of(&out), "cached output must match the computed output");
}

/// A read-only verb must not create the ledger. The caches moved out of SQLite
/// precisely so `log` never opens it — if that regressed, every read command
/// would be paying for a database it has no reason to touch.
#[test]
fn a_read_only_run_never_opens_the_ledger() {
    let (repo, home) = fixture("noledger", 4);
    let db = home.join("db.sqlite");
    let _ = std::fs::remove_file(&db);

    for args in [&["log", "--stat"][..], &["log", "--oneline"][..], &["blame", "keep.txt"][..]] {
        let out = zvcs(&repo, &home, args);
        assert!(out.status.success(), "`{}`: {}", args.join(" "), String::from_utf8_lossy(&out.stderr));
        assert!(!db.exists(), "`{}` created the ledger", args.join(" "));
    }
}

/// `-n` bounds the walk. A repository with more commits than the limit warms
/// only the newest ones — the recent end is what gets read.
#[test]
fn the_limit_bounds_how_much_is_warmed() {
    let (repo, home) = fixture("limit", 9);

    let out = zvcs(&repo, &home, &["zprecache", "-n", "4"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout_of(&out).trim(), "4", "the walk stops at the limit");
}

/// `-q` reports nothing on success: the daemon runs this on every ref change and
/// a count on stdout would be noise.
#[test]
fn quiet_prints_nothing() {
    let (repo, home) = fixture("quiet", 3);

    let out = zvcs(&repo, &home, &["zprecache", "-q"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout_of(&out).is_empty(), "quiet is silent: {:?}", stdout_of(&out));
}

/// A repository with no commits yet has nothing to warm, and saying so must not
/// be an error — the daemon calls this on any watched repo, including a fresh
/// `init` whose first ref write is what woke it up.
#[test]
fn an_unborn_head_warms_nothing_without_failing() {
    let root = std::env::temp_dir().join(format!("zvcs-precache-unborn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let home = root.join("home");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    std::fs::create_dir_all(&home).expect("mkdir home");
    assert!(zvcs(&repo, &home, &["init", "-q", "-b", "main"]).status.success(), "init");

    let out = zvcs(&repo, &home, &["zprecache"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout_of(&out).trim(), "0");
}
