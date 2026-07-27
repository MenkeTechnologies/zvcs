//! `git blame -C` credits lines that were copied out of *another* file.
//!
//! Each further `-C` widens the set of files in the parent that a leftover chunk is compared
//! against, which is the only thing that separates the three levels — so the fixture below is
//! built so that exactly one level finds each of its three copies:
//!
//! | blamed file | found by      | because the source file is                                |
//! |-------------|---------------|-----------------------------------------------------------|
//! | `moved`     | `-C`          | on the parent side of the commit's own tree diff           |
//! | `fresh`     | `-C -C`       | untouched by the commit, and the blamed path is new to it  |
//! | `grown`     | `-C -C -C`    | untouched, and the blamed path is *not* new to the parent  |
//!
//! The `-C` score also matters: a chunk is only handed over when `blame_entry_score()` — one plus
//! the alphanumeric bytes of the chunk — exceeds `sb->copy_score`, so every copied block here is
//! comfortably wordy and the last case checks that a `-C<score>` above it suppresses the copy.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Identity env vars git honors ABOVE `user.name`/`user.email` config; a CI runner that exports
/// them would otherwise author every commit here.
const IDENTITY_ENV: [&str; 4] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
];

/// Four lines wordy enough to clear `BLAME_DEFAULT_COPY_SCORE` (40) on their own.
const COPIED_BLOCK: &str = "library helper alpha computes the sum\n\
                            library helper beta computes the product\n\
                            library helper gamma computes the ratio\n\
                            library helper delta computes the mean\n";

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let mut cmd = Command::new(BIN);
    for var in IDENTITY_ENV {
        cmd.env_remove(var);
    }
    let ok = cmd
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

fn commit(dir: &Path, home: &Path, message: &str) {
    git(dir, home, &["add", "-A"]);
    git(dir, home, &["commit", "-q", "-m", message]);
}

/// The *Source File* column of each blame line, or `None` where blame did not print one.
///
/// `-f` forces the column on so that a line still attributed to the blamed path is distinguishable
/// from one whose source is another file, which is exactly what `-C` changes.
fn source_files(dir: &Path, home: &Path, extra: &[&str], file: &str) -> Vec<String> {
    let mut args = vec!["blame", "-f"];
    args.extend_from_slice(extra);
    args.push("--");
    args.push(file);
    let mut cmd = Command::new(BIN);
    for var in IDENTITY_ENV {
        cmd.env_remove(var);
    }
    let out = cmd
        .args(&args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "blame {extra:?} {file} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        // `^b957… lib.txt (alice 2026-… 1) text` → the token after the object name.
        .map(|line| {
            line.split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

#[test]
fn each_c_level_widens_which_file_a_copy_is_found_in() {
    let root = std::env::temp_dir().join(format!("zvcs-blamec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "a@e.co"]);
    git(&repo, &home, &["config", "user.name", "alice"]);

    // c1: the two files every copy below comes out of.
    std::fs::write(repo.join("lib"), COPIED_BLOCK).unwrap();
    std::fs::write(
        repo.join("notes"),
        "unrelated notes about the project scope\n",
    )
    .unwrap();
    std::fs::write(repo.join("grown"), "application entry point starts here\n").unwrap();
    commit(&repo, &home, "c1");

    // c2: `grown` gains the block; `lib` is left alone, and `grown` already existed in the parent,
    // so only "find copies harder for everybody" (`-C -C -C`) considers `lib` at all.
    std::fs::write(
        repo.join("grown"),
        format!("application entry point starts here\n{COPIED_BLOCK}"),
    )
    .unwrap();
    commit(&repo, &home, "c2");

    // c3: `fresh` is a brand-new path holding the block; `lib` is again left alone, but the parent
    // does not have `fresh` under any name, which is the `-C -C` case.
    std::fs::write(repo.join("fresh"), COPIED_BLOCK).unwrap();
    commit(&repo, &home, "c3");

    // c4: `moved` takes lines *out of* `notes` in the same commit, so `notes` is on the parent side
    // of this commit's own tree diff — a plain `-C` finds it.
    std::fs::write(
        repo.join("notes"),
        "unrelated notes about the project scope\n\
         fresh paragraph one describing the intended scope\n\
         fresh paragraph two describing the accepted risks\n\
         fresh paragraph three describing the delivery plan\n",
    )
    .unwrap();
    commit(&repo, &home, "c4");
    std::fs::write(
        repo.join("notes"),
        "unrelated notes about the project scope\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("moved"),
        "fresh paragraph one describing the intended scope\n\
         fresh paragraph two describing the accepted risks\n\
         fresh paragraph three describing the delivery plan\n",
    )
    .unwrap();
    commit(&repo, &home, "c5");

    // Without `-C` nothing is ever credited to another file.
    for file in ["moved", "fresh", "grown"] {
        let plain = source_files(&repo, &home, &[], file);
        assert!(
            plain.iter().all(|source| source == file),
            "plain blame of {file} should stay on {file}, got {plain:?}"
        );
    }

    // `-C`: only the file the commit itself removed lines from.
    assert_eq!(
        source_files(&repo, &home, &["-C"], "moved"),
        vec!["notes"; 3],
        "-C should credit `moved` to the lines it took out of `notes`"
    );
    assert!(
        source_files(&repo, &home, &["-C"], "fresh")
            .iter()
            .all(|source| source == "fresh"),
        "-C must not reach a file the commit left alone"
    );
    assert!(
        source_files(&repo, &home, &["-C"], "grown")
            .iter()
            .all(|source| source == "grown"),
        "-C must not reach a file the commit left alone"
    );

    // `-C -C`: also an untouched file, but only while the blamed path is new to the parent.
    assert_eq!(
        source_files(&repo, &home, &["-C", "-C"], "fresh"),
        vec!["lib"; 4],
        "-C -C should credit the new file `fresh` to the untouched `lib`"
    );
    assert!(
        source_files(&repo, &home, &["-C", "-C"], "grown")
            .iter()
            .all(|source| source == "grown"),
        "-C -C must not reach an untouched file for a path the parent already has"
    );

    // `-C -C -C`: untouched files for everybody. Line 1 predates the copy and stays put.
    assert_eq!(
        source_files(&repo, &home, &["-C", "-C", "-C"], "grown"),
        vec!["grown", "lib", "lib", "lib", "lib"],
        "-C -C -C should credit the block appended to `grown` to `lib`"
    );

    // `-C<score>` above the chunk's `blame_entry_score()` suppresses the copy; the four copied
    // lines score 1 + their alphanumeric bytes, which is well under 100000.
    assert_eq!(
        source_files(&repo, &home, &["-C100000"], "moved"),
        vec!["moved"; 3],
        "a copy score the chunk cannot beat must leave the lines with the blamed file"
    );

    // git reads the score with `strtoul` over the *whole* attached argument, so `-CC` is a single
    // `-C` whose score `\"C\"` is unparsable (and therefore the default), not two `-C`s.
    assert!(
        source_files(&repo, &home, &["-CC"], "fresh")
            .iter()
            .all(|source| source == "fresh"),
        "-CC is one -C with an unparsable score, not -C -C"
    );

    let _ = std::fs::remove_dir_all(&root);
}
