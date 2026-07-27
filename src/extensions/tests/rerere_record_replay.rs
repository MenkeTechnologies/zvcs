//! `rerere` must record a conflict's preimage, learn the resolution, and replay
//! it on the next identical conflict — the half of `rerere.c` `git merge` drives.
//!
//! The conflict id is a SHA-1 of the *normalised* conflict hunk (each side, NUL
//! separated, with the diff3 ancestor section dropped and the two sides ordered
//! lexicographically). Nothing else in the tree derives it, so a regression in
//! the normaliser is invisible except here: a wrong id files the record under a
//! directory the next run never looks in, and the replay silently stops
//! happening while every command still exits 0. Each step below therefore
//! asserts on the `rr-cache` layout *and* the message, since either alone can
//! pass while the other is broken.
//!
//! `--rerere-autoupdate` and `rerere.autoupdate` are the same replay with the
//! result staged; the index stage is what separates them from the plain replay.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(BIN).args(args).current_dir(repo).output().unwrap()
}

fn git(repo: &Path, args: &[&str]) {
    let out = run(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Two branches that change the same line differently, so merging them always
/// produces the same one-hunk conflict — hence always the same conflict id.
fn fixture(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("zvcs-rrec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();

    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "alice@example.com"]);
    git(&repo, &["config", "user.name", "Alice"]);
    git(&repo, &["config", "rerere.enabled", "true"]);

    std::fs::write(repo.join("f"), "a\nb\nc\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "base"]);

    git(&repo, &["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.join("f"), "a\nSIDE\nc\n").unwrap();
    git(&repo, &["commit", "-q", "-a", "-m", "side"]);

    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f"), "a\nMAIN\nc\n").unwrap();
    git(&repo, &["commit", "-q", "-a", "-m", "main"]);
    repo
}

/// The single `rr-cache/<id>` directory, which only exists once a conflict has
/// been recorded. Its name *is* the conflict id, so a stable name across runs is
/// what proves the normaliser is deterministic.
fn only_entry(repo: &Path) -> PathBuf {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(repo.join(".git/rr-cache"))
        .expect("rr-cache exists once rerere is enabled")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(dirs.len(), 1, "expected exactly one conflict id, got {dirs:?}");
    dirs.pop().unwrap()
}

/// `MERGE_RR` records are `<id>[.<variant>] HT <path> NUL`.
fn merge_rr(repo: &Path) -> Vec<u8> {
    std::fs::read(repo.join(".git/MERGE_RR")).unwrap_or_default()
}

/// The `XY path` line `git status --short` prints for `f`, e.g. `UU` while the
/// path is unmerged and `M ` once it is staged at stage #0.
fn status_of_f(repo: &Path) -> String {
    let out = run(repo, &["status", "--short"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.ends_with(" f"))
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn merge_records_a_preimage_and_replays_the_recorded_resolution() {
    let repo = fixture("replay");

    // --- the conflict: a preimage is recorded, and MERGE_RR names the path ---
    let out = run(&repo, &["merge", "side"]);
    assert_eq!(out.status.code(), Some(1), "a conflicted merge exits 1");
    assert!(
        stderr_of(&out).contains("Recorded preimage for 'f'"),
        "no preimage was recorded: {}",
        stderr_of(&out)
    );

    let entry = only_entry(&repo);
    let id = entry.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(entry.join("preimage").is_file(), "preimage missing in {entry:?}");
    assert!(
        !entry.join("postimage").exists(),
        "an unresolved conflict must not have a postimage yet"
    );

    // The preimage is the *normalised* conflict: no branch labels on the
    // markers, and no diff3 ancestor section.
    assert_eq!(
        std::fs::read_to_string(entry.join("preimage")).unwrap(),
        "a\n<<<<<<<\nMAIN\n=======\nSIDE\n>>>>>>>\nc\n"
    );

    let mut expected_rr = id.clone().into_bytes();
    expected_rr.extend_from_slice(b"\tf\0");
    assert_eq!(merge_rr(&repo), expected_rr, "MERGE_RR must bind the id to 'f'");

    // --- the resolution: `git rerere` turns the worktree file into a postimage
    let resolution = "a\nRESOLVED\nc\n";
    std::fs::write(repo.join("f"), resolution).unwrap();
    let out = run(&repo, &["rerere"]);
    assert!(out.status.success(), "git rerere failed: {}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("Recorded resolution for 'f'."),
        "the resolution was not recorded: {}",
        stderr_of(&out)
    );
    assert_eq!(
        std::fs::read_to_string(entry.join("postimage")).unwrap(),
        resolution,
        "the postimage is the worktree file verbatim"
    );
    assert!(
        merge_rr(&repo).is_empty(),
        "a fully recorded session leaves MERGE_RR empty"
    );

    // --- the replay: the same conflict resolves itself, still unmerged in the
    // index because nothing asked for it to be staged.
    git(&repo, &["merge", "--abort"]);
    let out = run(&repo, &["merge", "side"]);
    assert_eq!(out.status.code(), Some(1), "the merge still conflicts");
    assert!(
        stderr_of(&out).contains("Resolved 'f' using previous resolution."),
        "the recorded resolution was not replayed: {}",
        stderr_of(&out)
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("f")).unwrap(),
        resolution,
        "the replay must write the recorded resolution to the worktree"
    );
    assert_eq!(status_of_f(&repo), "UU f", "a plain replay leaves the path unmerged");

    // Replaying must not mint a second conflict id for the same conflict.
    assert_eq!(only_entry(&repo).file_name().unwrap().to_str().unwrap(), id);
}

#[test]
fn autoupdate_stages_the_replayed_resolution() {
    let repo = fixture("autoupdate");

    // Record the conflict and its resolution.
    run(&repo, &["merge", "side"]);
    std::fs::write(repo.join("f"), "a\nRESOLVED\nc\n").unwrap();
    git(&repo, &["rerere"]);
    git(&repo, &["merge", "--abort"]);

    // `--rerere-autoupdate` replays *and* stages, so the path leaves stage #0.
    let out = run(&repo, &["merge", "--rerere-autoupdate", "side"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr_of(&out).contains("Staged 'f' using previous resolution."),
        "--rerere-autoupdate did not stage the replay: {}",
        stderr_of(&out)
    );
    assert_eq!(status_of_f(&repo), "M  f", "the replay must be staged");

    // `rerere.autoupdate` is the same behaviour as a configured default...
    git(&repo, &["merge", "--abort"]);
    git(&repo, &["config", "rerere.autoupdate", "true"]);
    let out = run(&repo, &["merge", "side"]);
    assert!(
        stderr_of(&out).contains("Staged 'f' using previous resolution."),
        "rerere.autoupdate was not honoured: {}",
        stderr_of(&out)
    );
    assert_eq!(status_of_f(&repo), "M  f");

    // ...and `--no-rerere-autoupdate` overrides it back for the one run.
    git(&repo, &["merge", "--abort"]);
    let out = run(&repo, &["merge", "--no-rerere-autoupdate", "side"]);
    assert!(
        stderr_of(&out).contains("Resolved 'f' using previous resolution."),
        "--no-rerere-autoupdate did not override rerere.autoupdate: {}",
        stderr_of(&out)
    );
    assert_eq!(status_of_f(&repo), "UU f");
}

#[test]
fn rerere_enabled_false_records_nothing() {
    let repo = fixture("disabled");
    git(&repo, &["config", "rerere.enabled", "false"]);

    let out = run(&repo, &["merge", "side"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        !stderr_of(&out).contains("Recorded preimage"),
        "rerere.enabled=false must record nothing: {}",
        stderr_of(&out)
    );
    assert!(
        !repo.join(".git/rr-cache").exists(),
        "rerere.enabled=false must not create rr-cache"
    );
    assert!(
        !repo.join(".git/MERGE_RR").exists(),
        "rerere.enabled=false must not write MERGE_RR"
    );
}
