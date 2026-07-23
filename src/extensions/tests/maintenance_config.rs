//! `git maintenance run` honors `maintenance.strategy` and
//! `maintenance.<task>.enabled` when selecting which tasks to run.
//!
//! `maintenance run` is silent on success, so these keys cannot be checked on
//! stdout/stderr the way a diagnostic-bearing config can. Their effect is which
//! tasks run, so every assertion here reads a task's *observable side effect* on
//! the repository and compares the port against the system `git` (2.55.0) run
//! with a byte-identical environment:
//!
//!   * **`pack-refs`** packs loose refs into `.git/packed-refs`; its presence is
//!     the signal that the task ran. It is a default-set task, so a bare run
//!     creates the file and `maintenance.pack-refs.enabled=false` suppresses it.
//!   * **`reflog-expire`** drops reflog entries older than `gc.reflogExpire`
//!     (90 days); an injected ancient `HEAD` entry is expired when the task runs
//!     and kept when it is disabled. This is a second default-set task, so it
//!     guards against the `enabled` gate being wired to `pack-refs` alone.
//!   * **`gc`** is *not* in the default set but is the sole member of the
//!     `incremental` strategy set, and it too packs refs. So
//!     `maintenance.strategy=incremental` produces `packed-refs` via `gc`, and
//!     adding `maintenance.gc.enabled=false` empties the set and suppresses it —
//!     exercising the strategy code path and the `enabled` gate together.
//!
//! A `--task=<name>` selection overrides the config gate, matching git: it runs
//! the named task even when its `enabled` flag is `false`.
//!
//! The reading lives in `porcelain::maintenance::plan`: `maintenance.strategy`
//! chooses the default set, and `maintenance.<task>.enabled` adds or removes any
//! task from it. `maintenance.auto` and `maintenance.<task>.schedule` are *not*
//! exercised here because the port does not act on them — `--auto` is unported
//! and a `--schedule` run selects nothing without an OS scheduler — so there is
//! no observable behavior to compare.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A zeroed 40-char object id, used to build a syntactically valid reflog line.
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// Run a system-`git` command in `dir`, asserting success. Used only to build
/// the fixture and to write `.git/config`, never as the behavior under test.
fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A one-commit repository with a spare loose branch (`b1`) so `pack-refs` has
/// something to pack, plus an isolated, empty `HOME` so no ambient global
/// `maintenance.*` config leaks into the run. `configs` is applied to the
/// repository's own `.git/config` before the run.
fn fixture(tag: &str, configs: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-maintcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "hello\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "c0"]);
    git(&repo, &["branch", "b1"]);
    for (k, v) in configs {
        git(&repo, &["config", k, v]);
    }
    (repo, home)
}

/// Run `git maintenance run [extra]` under a deterministic, isolated environment.
/// `bin` is either the zvcs binary or the system `git`, run with byte-identical
/// env so their observable effects are directly comparable.
fn run(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["maintenance", "run"];
    args.extend_from_slice(extra);
    Command::new(bin)
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

/// Whether `pack-refs` ran, i.e. whether it produced `.git/packed-refs`.
fn packed_refs(repo: &Path) -> bool {
    repo.join(".git/packed-refs").exists()
}

/// Prepend a reflog entry timestamped in 2001 to `.git/logs/HEAD`, old enough
/// that `reflog-expire`'s 90-day cutoff drops it. The line uses git's exact
/// `<old> <new> <ident> <secs> <tz>\t<message>` shape.
fn inject_ancient_reflog(repo: &Path) {
    let log = repo.join(".git/logs/HEAD");
    let existing = std::fs::read(&log).unwrap();
    let mut body = format!(
        "{ZERO_OID} {ZERO_OID} Alice <alice@example.com> 1000000000 +0000\tancient\n"
    )
    .into_bytes();
    body.extend_from_slice(&existing);
    std::fs::write(&log, body).unwrap();
}

/// Whether the injected ancient reflog entry survived the run.
fn reflog_has_ancient(repo: &Path) -> bool {
    std::fs::read_to_string(repo.join(".git/logs/HEAD"))
        .unwrap()
        .lines()
        .any(|l| l.contains(" 1000000000 "))
}

/// `maintenance.<task>.enabled=false` removes a default-set task, and the port
/// agrees with git on the resulting `pack-refs` side effect. A bare run packs
/// refs; disabling `pack-refs` leaves them loose.
#[test]
fn task_enabled_false_suppresses_default_pack_refs() {
    // Bare run: pack-refs is in the default set, so packed-refs appears.
    let (rz, hz) = fixture("packon-z", &[]);
    let (rg, hg) = fixture("packon-g", &[]);
    let z = run(BIN, &rz, &hz, &[]);
    let g = run("git", &rg, &hg, &[]);
    assert_eq!(z.status.code(), Some(0), "run must succeed");
    assert_eq!(g.status.code(), Some(0), "sanity: git succeeds");
    assert!(packed_refs(&rz), "default run packs refs");
    assert_eq!(packed_refs(&rz), packed_refs(&rg), "must match git");

    // Disabled: pack-refs is dropped from the set, so refs stay loose.
    let cfg = [("maintenance.pack-refs.enabled", "false")];
    let (rz, hz) = fixture("packoff-z", &cfg);
    let (rg, hg) = fixture("packoff-g", &cfg);
    let z = run(BIN, &rz, &hz, &[]);
    let g = run("git", &rg, &hg, &[]);
    assert_eq!(z.status.code(), Some(0));
    assert!(!packed_refs(&rz), "pack-refs.enabled=false leaves refs loose");
    assert!(rz.join(".git/refs/heads/b1").exists(), "loose ref remains present");
    assert_eq!(packed_refs(&rz), packed_refs(&rg), "must match git");

    cleanup(&[&rz, &rg]);
}

/// A `--task=<name>` selection overrides the `enabled` gate: `pack-refs` runs
/// even with `maintenance.pack-refs.enabled=false`, matching git.
#[test]
fn cli_task_selection_overrides_enabled_false() {
    let cfg = [("maintenance.pack-refs.enabled", "false")];
    let (rz, hz) = fixture("override-z", &cfg);
    let (rg, hg) = fixture("override-g", &cfg);
    let z = run(BIN, &rz, &hz, &["--task=pack-refs"]);
    let g = run("git", &rg, &hg, &["--task=pack-refs"]);
    assert_eq!(z.status.code(), Some(0));
    assert_eq!(g.status.code(), Some(0));
    assert!(packed_refs(&rz), "--task=pack-refs runs despite enabled=false");
    assert_eq!(packed_refs(&rz), packed_refs(&rg), "must match git");

    cleanup(&[&rz, &rg]);
}

/// `maintenance.reflog-expire.enabled` gates the `reflog-expire` task — a second
/// default-set task, so the gate is not special-cased to `pack-refs`. A bare run
/// expires an ancient entry; disabling the task keeps it. Byte-for-byte with git.
#[test]
fn reflog_expire_task_enabled_gate() {
    // Enabled (default): the ancient entry is expired.
    let (rz, hz) = fixture("reflog-on-z", &[]);
    let (rg, hg) = fixture("reflog-on-g", &[]);
    inject_ancient_reflog(&rz);
    inject_ancient_reflog(&rg);
    let z = run(BIN, &rz, &hz, &[]);
    let g = run("git", &rg, &hg, &[]);
    assert_eq!(z.status.code(), Some(0));
    assert_eq!(g.status.code(), Some(0), "sanity: git succeeds");
    assert!(!reflog_has_ancient(&rz), "reflog-expire drops the ancient entry");
    assert_eq!(reflog_has_ancient(&rz), reflog_has_ancient(&rg), "must match git");

    // Disabled: the task is removed, so the ancient entry survives.
    let cfg = [("maintenance.reflog-expire.enabled", "false")];
    let (rz, hz) = fixture("reflog-off-z", &cfg);
    let (rg, hg) = fixture("reflog-off-g", &cfg);
    inject_ancient_reflog(&rz);
    inject_ancient_reflog(&rg);
    let z = run(BIN, &rz, &hz, &[]);
    let g = run("git", &rg, &hg, &[]);
    assert_eq!(z.status.code(), Some(0));
    assert_eq!(g.status.code(), Some(0), "sanity: git succeeds");
    assert!(reflog_has_ancient(&rz), "disabled reflog-expire keeps the entry");
    assert_eq!(reflog_has_ancient(&rz), reflog_has_ancient(&rg), "must match git");

    cleanup(&[&rz, &rg]);
}

/// `maintenance.strategy=incremental` selects `gc` and nothing else; `gc` packs
/// refs, so `packed-refs` appears. Adding `maintenance.gc.enabled=false` empties
/// the set and suppresses it. This exercises the strategy code path and the
/// `enabled` gate together, and matches git in both states.
#[test]
fn strategy_incremental_runs_gc_and_enabled_gates_it() {
    // strategy=incremental → gc runs → refs packed.
    let cfg = [("maintenance.strategy", "incremental")];
    let (rz, hz) = fixture("strat-on-z", &cfg);
    let (rg, hg) = fixture("strat-on-g", &cfg);
    let z = run(BIN, &rz, &hz, &[]);
    let g = run("git", &rg, &hg, &[]);
    assert_eq!(z.status.code(), Some(0));
    assert_eq!(g.status.code(), Some(0));
    assert!(packed_refs(&rz), "strategy=incremental runs gc, which packs refs");
    assert_eq!(packed_refs(&rz), packed_refs(&rg), "must match git");

    // strategy=incremental + gc disabled → empty set → nothing packs refs.
    let cfg = [
        ("maintenance.strategy", "incremental"),
        ("maintenance.gc.enabled", "false"),
    ];
    let (rz, hz) = fixture("strat-off-z", &cfg);
    let (rg, hg) = fixture("strat-off-g", &cfg);
    let z = run(BIN, &rz, &hz, &[]);
    let g = run("git", &rg, &hg, &[]);
    assert_eq!(z.status.code(), Some(0));
    assert!(!packed_refs(&rz), "gc.enabled=false empties the strategy set");
    assert_eq!(packed_refs(&rz), packed_refs(&rg), "must match git");

    cleanup(&[&rz, &rg]);
}

/// Remove each fixture's root (the `repo/` parent), best-effort.
fn cleanup(repos: &[&PathBuf]) {
    for repo in repos {
        if let Some(parent) = repo.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}
