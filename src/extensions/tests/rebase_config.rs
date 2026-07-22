//! `git rebase` and its config keys — a regression guard for the ones that are
//! *not* ported, and why.
//!
//! `git help --config` lists these `rebase.*` keys beyond `rebase.backend`
//! (already read in `porcelain::rebase`): `rebase.stat`, `rebase.autoStash`,
//! `rebase.autoSquash`, `rebase.abbreviateCommands`, `rebase.missingCommitsCheck`
//! and `rebase.forkPoint`. Every one of them defaults a behavior that lives on a
//! path this port explicitly refuses, so none is mappable:
//!
//! * `rebase.stat` (git default: false) is the default for `--stat`/`--no-stat`,
//!   the upstream diffstat. `porcelain::rebase` refuses `--stat` up front (the
//!   diffstat is not ported), and git only prints it when a *divergent* range is
//!   actually replayed — the three-way-merge path this port also refuses. On
//!   every path it completes (up-to-date exit, forced exact replay, `--no-ff`
//!   noop) git prints no diffstat because nothing changed upstream, so the config
//!   has no observable effect.
//! * `rebase.autoSquash` (per git-config(1)) enables `--autosquash` "by default
//!   **for interactive mode**". It does *not* imply the merge backend or disable
//!   the preemptive fast-forward the way the command-line `--autosquash` does —
//!   `git -c rebase.autosquash=true rebase <up-to-date>` prints
//!   `Current branch <b> is up to date.`, identical to a plain rebase, whereas
//!   `git rebase --autosquash <up-to-date>` prints `Successfully rebased...`.
//!   Interactive rebase is not ported, so the config has no observable effect on
//!   any completed path — and wiring it to this port's `autosquash` variable
//!   (which *does* drive `imply_merge`/`allow_preemptive_ff`) would diverge from
//!   git.
//! * `rebase.autoStash` only matters against a dirty worktree (writing a stash
//!   commit), which this port refuses; on a clean tree neither git nor this port
//!   does anything with it.
//! * `rebase.forkPoint` only sets `--no-fork-point` when false; the `--fork-point`
//!   (true) side needs the upstream reflog walk this port refuses.
//! * `rebase.abbreviateCommands`/`rebase.missingCommitsCheck` govern the
//!   interactive todo list, which is not ported.
//!
//! These tests pin that non-effect: on the paths `porcelain::rebase` completes,
//! this binary matches the system `git` (2.55.0) byte for byte under every one of
//! those keys set to either value. That is the guard against a future change
//! wiring one of them into a variable it must not touch.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Run a system-`git` command in `dir`, asserting success. Used only to build
/// the fixture and to write `.git/config`, never as behavior under test.
fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
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
        let _ = Command::new("git")
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
    let real = run("git", repo, home, extra);
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
