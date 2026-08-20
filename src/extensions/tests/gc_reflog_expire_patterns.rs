//! `gc.<pattern>.reflogExpire` / `gc.<pattern>.reflogExpireUnreachable`, the
//! per-ref-pattern form `reflog_expire_config()` builds.
//!
//! The trap this guards is the one that made these keys wrong here before: a
//! matching pattern supplies **both** cutoffs, and the one it does not configure
//! is `0` — the `never` sentinel — rather than the global default.
//! `reflog_expire_options_set_refname()` (`reflog.c:107-115`) assigns
//! `ent->expire_total` and `ent->expire_unreachable` unconditionally once the
//! pattern matches, and `find_cfg_ent()` allocates `ent` with the zeroing
//! `FLEX_ALLOC_MEM`. So `gc.<refs/heads/*>.reflogExpireUnreachable=now` also
//! switches the *total* cutoff off, and a naive "fall back to the default"
//! reading expires entries git keeps.
//!
//! The fixture is one branch with three reflog entries, all backdated 400 days —
//! past both built-in cutoffs — of which only the tip is reachable. Counting the
//! surviving lines then separates the two cutoffs: 3 means "nothing expired",
//! 1 means "the unreachable ones went", 0 means "everything went".
//!
//! Every expected count was captured from git 2.55.0 (`/opt/homebrew/bin/git`)
//! on this exact fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

const REFLOG: &str = ".git/logs/refs/heads/main";

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run binary")
}

fn ok(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let out = run(cwd, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// One branch, three reflog entries (branch creation, one commit, one reset),
/// all backdated 400 days, with the second commit left unreachable.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-reflogpat-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    ok(&repo, &home, &["init", "-q", "-b", "main"]);
    ok(&repo, &home, &["config", "user.email", "alice@example.com"]);
    ok(&repo, &home, &["config", "user.name", "Alice"]);
    std::fs::write(repo.join("f"), "a\n").unwrap();
    ok(&repo, &home, &["add", "f"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c0"]);
    std::fs::write(repo.join("f"), "b\n").unwrap();
    ok(&repo, &home, &["add", "f"]);
    ok(&repo, &home, &["commit", "-q", "-m", "c1"]);
    ok(&repo, &home, &["reset", "-q", "--hard", "HEAD~1"]);

    backdate(&repo.join(REFLOG), 400 * 24 * 3600);
    assert_eq!(reflog_lines(&repo), 3, "fixture must start with three entries");
    (repo, home)
}

/// Move every timestamp in a reflog `seconds` into the past, leaving the rest of
/// each line untouched — the entries have to be older than both built-in cutoffs
/// for the configured ones to be what decides.
fn backdate(path: &Path, seconds: i64) {
    let text = std::fs::read_to_string(path).unwrap();
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        // `<old> <new> <name> <email> <ts> <tz>\t<message>`; the timestamp is the
        // last whitespace-separated field before the timezone.
        let (head, tail) = line.split_once('\t').unwrap_or((line, ""));
        let mut fields: Vec<String> = head.split(' ').map(str::to_string).collect();
        let tz = fields.len() - 1;
        let ts = tz - 1;
        let moved: i64 = fields[ts].parse::<i64>().unwrap() - seconds;
        fields[ts] = moved.to_string();
        out.push_str(&fields.join(" "));
        if !tail.is_empty() {
            out.push('\t');
            out.push_str(tail);
        }
        out.push('\n');
    }
    std::fs::write(path, out).unwrap();
}

fn reflog_lines(repo: &Path) -> usize {
    std::fs::read_to_string(repo.join(REFLOG))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// Run `gc -q` with `cfg` and report how many reflog entries survived.
fn surviving(tag: &str, cfg: &[&str]) -> usize {
    let (repo, home) = fixture(tag);
    let mut args: Vec<&str> = cfg.to_vec();
    args.extend_from_slice(&["gc", "-q"]);
    let out = run(&repo, &home, &args);
    assert!(out.status.success(), "gc {cfg:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    let left = reflog_lines(&repo);
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    left
}

#[test]
fn a_matching_pattern_supplies_both_cutoffs_even_when_it_configures_one() {
    // The core of `reflog_expire_options_set_refname()`. Setting only the
    // unreachable half of a pattern leaves its *total* half at the zeroed
    // `never`, so nothing expires at all — where the plain
    // `gc.reflogExpireUnreachable=never` leaves the 30-day total cutoff in place
    // and every backdated entry still goes.
    assert_eq!(
        surviving("plain-unreach", &["-c", "gc.reflogExpireUnreachable=never"]),
        0,
        "the plain key leaves the total cutoff alone, so the old entries expire"
    );
    assert_eq!(
        surviving("pattern-unreach", &["-c", "gc.refs/heads/*.reflogExpireUnreachable=never"]),
        3,
        "a matching pattern also switches the total cutoff off"
    );
    assert_eq!(
        surviving("pattern-total", &["-c", "gc.refs/heads/*.reflogExpire=never"]),
        3,
        "and symmetrically for the other half"
    );
}

#[test]
fn a_pattern_cutoff_still_separates_reachable_from_unreachable() {
    // With the total half off and the unreachable half at `now`, exactly the
    // entries whose objects fell out of the graph go — one of the three here.
    // This is what proves the pattern's value is *used* rather than merely
    // matched.
    assert_eq!(
        surviving("pattern-unreach-now", &["-c", "gc.refs/heads/*.reflogExpireUnreachable=now"]),
        1,
        "only the unreachable entries expire"
    );
    assert_eq!(
        surviving("pattern-total-now", &["-c", "gc.refs/heads/*.reflogExpire=now"]),
        0,
        "a total cutoff of now takes everything, reachable or not"
    );
    assert_eq!(
        surviving(
            "pattern-both",
            &[
                "-c",
                "gc.refs/heads/*.reflogExpire=never",
                "-c",
                "gc.refs/heads/*.reflogExpireUnreachable=never",
            ],
        ),
        3,
        "both halves set on one pattern keep everything"
    );
}

#[test]
fn a_pattern_that_does_not_match_leaves_the_defaults_in_place() {
    // `wildmatch(ent->pattern, ref, 0)`: no match means the entry contributes
    // nothing and the built-in cutoffs decide, so the backdated entries go.
    assert_eq!(
        surviving("pattern-nomatch", &["-c", "gc.refs/tags/*.reflogExpireUnreachable=never"]),
        0,
        "refs/tags/* does not match refs/heads/main"
    );
    // A bare `*` does, since git's `wildmatch` is called with no flags and `*`
    // spans `/`.
    assert_eq!(
        surviving("pattern-star", &["-c", "gc.*.reflogExpireUnreachable=never"]),
        3,
        "* spans the slashes in a full ref name"
    );
}

#[test]
fn a_matching_pattern_beats_the_plain_keys() {
    // The pattern loop runs before the "nothing matched" fallback, and it assigns
    // both slots, so a plain key set alongside a matching pattern does not get a
    // say. Here the plain `gc.reflogExpire=never` would keep one entry on its own
    // (the reachable tip); with the pattern in play all three survive, because
    // the pattern's unset total half is `never` too.
    assert_eq!(surviving("plain-total-only", &["-c", "gc.reflogExpire=never"]), 1);
    assert_eq!(
        surviving(
            "plain-plus-pattern",
            &[
                "-c",
                "gc.reflogExpire=never",
                "-c",
                "gc.refs/heads/*.reflogExpireUnreachable=never",
            ],
        ),
        3,
        "the matching pattern supplies both cutoffs, so the plain key is not consulted"
    );
}
