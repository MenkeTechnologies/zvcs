//! `git fetch` between local repositories, on the reporting side: the summary
//! rows, `FETCH_HEAD`, the refs and the `--set-upstream` warnings, each case run
//! by stock git and by zvcs on twin copies of one fixture at the same path.
//!
//! * `--prune-tags` is dropped when a refspec follows the remote
//!   (`prune_tags_ok = (argc == 1)`, builtin/fetch.c:2778), and for a URL its tag
//!   refspec joins the command line's (:2464-2469), so there is no `HEAD`
//!   fallback and every `FETCH_HEAD` row is a merge candidate.
//! * `fetch.output=compact` replaces a name only on whole `/`-separated
//!   components (`find_and_replace()`, :780-805): `HEAD -> FETCH_HEAD` stays.
//! * A command-line `refs/tags/*:refs/tags/*` is a ref-map entry, not tag
//!   following, so its new tags are fetched and listed in advertisement order.
//! * A destination-less opportunistic entry (`--refmap=''`, or a configured
//!   refspec without one) is reported a second time as `* branch ... ->
//!   FETCH_HEAD` but is `FETCH_HEAD_IGNORE` in the file (:1176-1178, :1353-1366).
//! * `--set-upstream` counts only ref-map entries without a peer ref, and its
//!   warnings are git's text (:2076-2127).
//!
//! No network: the remote is a directory beside the clone.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .output()
        .expect("spawn git")
}

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str]) {
    let out = run(bin, dir, home, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `up` (main, feat; tags v1, moving) cloned into `cl` with its remote spelled
/// `../up`, then advanced: a new commit on main, a lightweight `v2`, an annotated
/// `a2` and `moving` moved onto the new commit. `cl` adds a local-only tag.
fn base(stock: &str, root: &Path, home: &Path) -> PathBuf {
    let base = root.join("base");
    let up = base.join("up");
    std::fs::create_dir_all(&up).unwrap();
    let g = |dir: &Path, args: &[&str]| ok(stock, dir, home, args);
    g(&up, &["init", "-q", "-b", "main"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "one"]);
    g(&up, &["branch", "feat"]);
    g(&up, &["tag", "v1"]);
    g(&up, &["tag", "moving"]);
    g(&base, &["clone", "-q", "up", "cl"]);
    let cl = base.join("cl");
    g(&cl, &["config", "remote.origin.url", "../up"]);
    g(&cl, &["tag", "zzlocal"]);
    g(&up, &["commit", "-q", "--allow-empty", "-m", "two"]);
    g(&up, &["tag", "v2"]);
    g(&up, &["tag", "-a", "-m", "ann", "a2"]);
    g(&up, &["tag", "-f", "moving"]);
    base
}

/// Everything a case can change, read back with stock git.
fn state(stock: &str, cl: &Path, home: &Path) -> String {
    let refs = run(stock, cl, home, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    let config = run(stock, cl, home, &["config", "--local", "--list"]);
    let fetch_head = std::fs::read_to_string(cl.join(".git/FETCH_HEAD")).unwrap_or_default();
    format!(
        "{}--\n{}--\n{fetch_head}",
        String::from_utf8_lossy(&refs.stdout),
        String::from_utf8_lossy(&config.stdout)
    )
}

/// Runs `cases` (`sh -c` scripts with `$G` naming the binary) from `cl` under
/// both binaries, each on a fresh copy of the fixture at the same path.
fn compare(tag: &str, cases: &[&str]) {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-fetchsweep-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _scratch = Scratch(root.clone());
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".gitconfig"), "[maintenance]\n\tauto = false\n[gc]\n\tauto = 0\n").unwrap();
    let base = base(stock, &root, &home);
    let work = root.join("w");
    let cl = work.join("cl");
    for case in cases {
        let mut seen = Vec::new();
        for bin in [stock, BIN] {
            let _ = std::fs::remove_dir_all(&work);
            let cp = Command::new("cp").arg("-R").arg(&base).arg(&work).status().unwrap();
            assert!(cp.success());
            let out = Command::new("sh")
                .arg("-c")
                .arg(case)
                .current_dir(&cl)
                .env("G", bin)
                .env("S", stock)
                .env("HOME", &home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
                .env_remove("XDG_CONFIG_HOME")
                .env("GIT_EDITOR", "true")
                .env("GIT_AUTHOR_NAME", "A U Thor")
                .env("GIT_AUTHOR_EMAIL", "author@example.com")
                .env("GIT_COMMITTER_NAME", "C O Mitter")
                .env("GIT_COMMITTER_EMAIL", "committer@example.com")
                .env("GIT_AUTHOR_DATE", "1700000100 +0000")
                .env("GIT_COMMITTER_DATE", "1700000100 +0000")
                .env("LC_ALL", "C")
                .env("TERM", "dumb")
                .output()
                .unwrap();
            seen.push(format!(
                "rc={:?}\n--stdout\n{}--stderr\n{}--state\n{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
                state(stock, &cl, &home)
            ));
        }
        assert_eq!(seen[1], seen[0], "case {case:?}: zvcs (left) vs stock (right)");
    }
}

#[test]
fn prune_tags_with_a_url_or_a_refspec() {
    compare(
        "prunetags",
        &[
            "$G fetch -p -P -v ../up",
            "$G fetch -p -P origin main",
        ],
    );
}

#[test]
fn compact_output_and_command_line_tag_refspecs() {
    compare(
        "compact",
        &[
            "$G -c fetch.output=compact fetch ../up",
            "$G fetch -v origin 'refs/tags/*:refs/tags/*'",
        ],
    );
}

#[test]
fn destination_less_opportunistic_entries() {
    compare(
        "refmap",
        &[
            "$G fetch --refmap= origin HEAD",
            "$S config --add remote.origin.fetch feat && $G fetch origin feat",
        ],
    );
}

#[test]
fn rejections_and_the_second_tag_pass() {
    compare(
        "rejections",
        &[
            // `-u` lets the checked-out branch be named, but a non-fast-forward is
            // still `[rejected]`, so the fetch fails and the summary loses its width.
            "$S commit -q --allow-empty -m local && $G fetch -u origin main:refs/heads/main",
            // The new tags point at a commit this fetch does not bring, so there is no
            // `backfill_tags()` pass and the forced-update note is printed once.
            "$G fetch --no-show-forced-updates origin feat:refs/remotes/origin/feat",
        ],
    );
}

#[test]
fn set_upstream_warnings() {
    compare(
        "upstream",
        &[
            "$G fetch --set-upstream origin",
            "$S checkout -q --detach && $G fetch --set-upstream ../up main",
        ],
    );
}
