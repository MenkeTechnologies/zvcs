//! `git rebase`'s config keys on the paths that replay *nothing* — the
//! up-to-date exit and the forced exact replay of an already-based range.
//!
//! Those two paths run before the sequencer and are where a mis-wired key does
//! the most damage, because none of them should change anything there: git
//! prints no upstream diffstat when nothing changed upstream, has no dirty tree
//! to autostash, and has no instruction sheet to abbreviate or reorder. These
//! tests pin that non-effect against the system `git`, byte for byte, with each
//! key set to either value.
//!
//! One key needs its own case. `rebase.autoSquash` (per git-config(1)) enables
//! `--autosquash` "by default **for interactive mode**", and unlike the
//! command-line flag it must *not* imply the merge backend or disable the
//! preemptive fast-forward — `cmd_rebase()` resolves the config only after
//! `allow_preemptive_ff` has been decided. So
//! `git -c rebase.autosquash=true rebase <up-to-date>` prints
//! `Current branch <b> is up to date.` while
//! `git rebase --autosquash <up-to-date>` prints `Successfully rebased…`, and
//! [`rebase_autosquash_config_keeps_preemptive_fast_forward`] is the guard
//! against collapsing the two.
//!
//! What these keys *do* once a sheet exists — `rebase.autoSquash`,
//! `rebase.abbreviateCommands`, `rebase.instructionFormat`,
//! `rebase.missingCommitsCheck` and `rebase.rescheduleFailedExec` — is covered
//! in `rebase_interactive.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A STOCK git to compare against, or `None` when the machine has no foreign git
/// installed.
///
/// These are differential tests: their whole point is to diff zvcs against
/// another implementation, so they are the one place a foreign binary is
/// legitimate. It is resolved EXPLICITLY (`ZVCS_STOCK_GIT`, else the system
/// path) rather than through `PATH`, because on a machine where zvcs shadows
/// git — the machine this is developed on — `PATH` resolution silently makes the
/// oracle the thing under test, and the comparison proves nothing. When no stock
/// git exists the oracle half is skipped and the zvcs-side assertions still run.
fn stock_git() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return std::path::Path::new(&p).exists().then_some(p);
    }
    ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(str::to_owned)
}

/// Run a system-`git` command in `dir`, asserting success. Used only to build
/// the fixture and to write `.git/config`, never as behavior under test.
fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A repo whose `topic` branch already sits on top of `main` (up to date), plus
/// an isolated empty `HOME` so no ambient global `rebase.*` config leaks in.
///
/// Linear history `c0 → c1 → c2` on `main`, `topic` branched at the tip. A
/// `rebase main` from `topic` is therefore up to date; `rebase -f main` is the
/// forced exact-replay of the `main..topic` range (empty here) onto `main`.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-rbcfg-{tag}-{}", std::process::id()));
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
    for (n, body) in [("c0", "a\n"), ("c1", "a\nb\n"), ("c2", "a\nb\nc\n")] {
        std::fs::write(repo.join("f"), body).unwrap();
        git(&repo, &["add", "f"]);
        git(&repo, &["commit", "-q", "-m", n]);
    }
    git(&repo, &["branch", "topic"]);
    git(&repo, &["checkout", "-q", "topic"]);
    (repo, home)
}

/// Reset `topic` back onto `main` and drop any `rebase.*` config, so each case
/// starts from the pristine up-to-date state.
fn reset(repo: &Path) {
    git(repo, &["checkout", "-q", "topic"]);
    git(repo, &["reset", "-q", "--hard", "main"]);
    for key in ["rebase.stat", "rebase.autosquash", "rebase.autostash", "rebase.forkpoint"] {
        // `--unset-all` on an absent key exits 5; ignore it.
        let _ = Command::new(BIN)
            .args(["config", "--unset-all", key])
            .current_dir(repo)
            .status();
    }
}

/// Run `<bin> rebase <extra>` under a deterministic, isolated environment. `bin`
/// is either this binary or the system `git`, run with byte-identical env so the
/// outputs compare directly.
fn run(bin: &str, repo: &Path, home: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["rebase"];
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

fn triple(o: &Output) -> (Vec<u8>, Vec<u8>, i32) {
    (o.stdout.clone(), o.stderr.clone(), o.status.code().unwrap_or(-1))
}

fn show(label: &str, o: &Output) -> String {
    format!(
        "{label}: exit={:?}\n  stdout={:?}\n  stderr={:?}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
    )
}

/// With `rebase.<key>=<val>` in `.git/config`, `<bin> rebase <extra>` from
/// `topic` must produce byte-identical stdout, stderr and exit for this binary
/// and the system `git`.
fn assert_matches_git(repo: &Path, home: &Path, key: &str, val: &str, extra: &[&str]) {
    reset(repo);
    git(repo, &["config", key, val]);
    let real = run(&stock_git().unwrap_or_else(|| "/usr/bin/git".into()), repo, home, extra);
    reset(repo);
    git(repo, &["config", key, val]);
    let zvcs = run(BIN, repo, home, extra);
    assert_eq!(
        triple(&zvcs),
        triple(&real),
        "{key}={val} `rebase {extra:?}` diverged from git\n{}\n{}",
        show("zvcs", &zvcs),
        show("git", &real),
    );
}

/// The up-to-date exit is unaffected by any of the four value-keyed keys, in
/// either direction — exactly as git leaves it.
#[test]
fn config_keys_do_not_alter_up_to_date_exit() {
    let (repo, home) = fixture("uptodate");
    for key in ["rebase.stat", "rebase.autosquash", "rebase.autostash", "rebase.forkpoint"] {
        for val in ["true", "false"] {
            assert_matches_git(&repo, &home, key, val, &["main"]);
        }
    }
}

/// The forced exact-replay path (`-f`, an up-to-date range re-committed onto the
/// same base) prints no upstream diffstat, so `rebase.stat=true` leaves it
/// byte-identical to git — the guard against defaulting `--stat` on here.
#[test]
fn rebase_stat_true_does_not_add_diffstat_on_forced_replay() {
    let (repo, home) = fixture("statforced");
    assert_matches_git(&repo, &home, "rebase.stat", "true", &["-f", "main"]);
}

/// `rebase.autosquash=true` must behave like git's config, not like the
/// command-line `--autosquash`: it neither implies the merge backend nor
/// disables the preemptive fast-forward, so the up-to-date exit stays
/// `Current branch <b> is up to date.` rather than a sequencer finish.
#[test]
fn rebase_autosquash_config_keeps_preemptive_fast_forward() {
    let (repo, home) = fixture("autosquash");
    reset(&repo);
    git(&repo, &["config", "rebase.autosquash", "true"]);
    let zvcs = run(BIN, &repo, &home, &["main"]);
    let out = String::from_utf8_lossy(&zvcs.stdout);
    assert!(
        out.contains("is up to date.") && !out.contains("Successfully rebased"),
        "rebase.autosquash=true wrongly changed the up-to-date exit\n{}",
        show("zvcs", &zvcs),
    );
    assert_eq!(zvcs.status.code(), Some(0));
}
