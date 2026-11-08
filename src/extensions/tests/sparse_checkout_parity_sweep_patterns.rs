//! `git sparse-checkout` against stock git, each scenario run on twin fixtures built
//! by stock git and compared on stdout, stderr, exit code and the state left behind:
//! the index as stock lists it, the pattern file, the config and the worktree.
//!
//! - A worktree whose stat data no longer matches the index (a fixture copied with
//!   `cp -R`) keeps the paths a narrowing `set` would remove: `verify_uptodate_1()`
//!   (unpack-trees.c:2224-2270) asks `ie_match_stat()` and compares content only for
//!   a racily clean entry, so a stat difference alone is "not up to date". An entry
//!   that was already skip-worktree is left alone with whatever file sits at its path
//!   (`apply_sparse_checkout()`, unpack-trees.c:553-564).
//! - `strbuf_to_cone_pattern()` (builtin/sparse-checkout.c:559-575) trims the
//!   whitespace around a cone path; only a standard-input line is C-unquoted; with
//!   `--stdin` the arguments are not patterns; `--` and `--end-of-options` end the
//!   options; a non-cone `--stdin` keeps its empty lines.
//! - A cone-mode pattern file `add_pattern_to_hashsets()` (dir.c:798-936) cannot read
//!   draws `unrecognized pattern` and `disabling cone pattern matching` at each load
//!   and is then matched as a non-cone list: `list` prints the patterns, `add` dies
//!   `existing sparse-checkout patterns do not use cone mode`, `init` and `reapply`
//!   load it twice. A lone `/*` is the full cone, `core.ignoreCase` folds the names.
//! - `check-rules` asks about every line, an empty one included, and reads a rules
//!   file line by line even under `-z`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sh(git: &str, root: &Path, dir: &Path, script: &str) -> (String, String, i32) {
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .env("G", git)
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    let clean = |b: &[u8]| String::from_utf8_lossy(b).replace(&root.display().to_string(), "ROOT").replace(git, "GIT");
    (clean(&out.stdout), clean(&out.stderr), out.status.code().unwrap_or(-1))
}

/// `src/repo`: root files and nested directories, `other` a second branch, and
/// `core.ignoreCase` pinned so the fixture reads the same on every filesystem.
const FIXTURE: &str = r#"set -e
c() { "$G" -c maintenance.auto=false "$@"; }
mkdir -p home src/repo
printf '[maintenance]\n\tauto = false\n' > home/.gitconfig
cd src/repo
c init -q -b main
c config core.ignoreCase false
mkdir -p a/x b c/d/e deep/1/2/3
for f in top a/f a/x/g b/f c/f c/d/f c/d/e/f deep/1/f deep/1/2/3/f; do echo $f > $f; done
c add .
c commit -qm init
c branch other
"#;

/// The modes a scenario starts from.
const CONE: &str = "\"$G\" sparse-checkout set a c/d";
const NO_CONE: &str = "\"$G\" sparse-checkout set --no-cone '/a/' '*.x' '!/a/x/'";

const STATE: &str = r#"
echo "== ls-files"; "$S" ls-files -s -t --sparse
echo "== config"; cat .git/config .git/config.worktree 2>/dev/null
echo "== sparse"; cat .git/info/sparse-checkout
echo "== wt"; find . -path ./.git -prune -o -type f -print | LC_ALL=C sort | while read f; do echo "$f $(cksum < "$f")"; done
"#;

/// `prepare` runs with stock git on the fixture; `copied` then copies the repository
/// with `cp -R`, which leaves every entry's stat data stale, where otherwise stock
/// refreshes it.
fn run_twin(tag: &str, prepare: &str, copied: bool, scenario: &str) -> Option<[(String, String, i32); 2]> {
    let stock = stock_git()?;
    let mut results = Vec::new();
    for (side, git) in [("stock", stock), ("zvcs", BIN)] {
        let root = Root(std::env::temp_dir().join(format!("zvcs-sc-sweep-{tag}-{side}-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&root.0);
        std::fs::create_dir_all(&root.0).unwrap();
        let built = sh(stock, &root.0, &root.0, FIXTURE);
        assert_eq!(built.2, 0, "fixture failed: {built:?}");
        let src = root.0.join("src/repo");
        let prepared = sh(stock, &root.0, &src, prepare);
        assert_eq!(prepared.2, 0, "preparation failed: {prepared:?}");
        let copy = sh(stock, &root.0, &root.0, "cp -R src/repo repo");
        assert_eq!(copy.2, 0, "copy failed: {copy:?}");
        let repo = root.0.join("repo");
        if !copied {
            sh(stock, &root.0, &repo, "\"$G\" update-index -q --refresh");
        }
        let ran = sh(git, &root.0, &repo, scenario);
        let state = sh(stock, &root.0, &repo, &format!("S=\"$G\"; {STATE}"));
        results.push((format!("{}{}", ran.0, state.0), ran.1, ran.2));
    }
    let zvcs = results.pop().unwrap();
    let stock = results.pop().unwrap();
    Some([stock, zvcs])
}

fn assert_twin(tag: &str, prepare: &str, copied: bool, scenario: &str) {
    let Some([stock, zvcs]) = run_twin(tag, prepare, copied, scenario) else { return };
    assert_eq!(zvcs, stock, "scenario `{tag}` differs from stock git:\n{scenario}");
}

#[test]
fn stale_stat_data_keeps_the_paths_a_narrowing_would_remove() {
    assert_twin("stale-no-cone", "true", true, "$G sparse-checkout set --no-cone /a/; echo rc=$?");
    assert_twin("stale-cone", "true", true, "$G sparse-checkout set a; echo rc=$?");
    assert_twin("stale-init", "true", true, "$G sparse-checkout init --cone; echo rc=$?");
    assert_twin("stale-narrow", CONE, true, "$G sparse-checkout set --no-cone /a/; echo rc=$?");
}

#[test]
fn skipped_entries_are_left_alone() {
    assert_twin("skipped-present", CONE, false, "echo x > b/f; $G sparse-checkout reapply; echo rc=$?");
    assert_twin(
        "skipped-set",
        CONE,
        false,
        "mkdir -p b; echo x > b/f; $G sparse-checkout set a c; echo rc=$?",
    );
}

#[test]
fn cone_arguments_and_standard_input() {
    for (tag, scenario) in [
        ("trim", "$G sparse-checkout set ' a'; echo rc=$?; $G sparse-checkout list"),
        ("argv-quote", "$G sparse-checkout set '\"a\"'; echo rc=$?; $G sparse-checkout list"),
        ("stdin-quote", "printf '\"a\"\\n \" b\"\\n' | $G sparse-checkout set --stdin; echo rc=$?; $G sparse-checkout list"),
        ("stdin-args", "printf 'b\\n' | $G sparse-checkout set --stdin extra; echo rc=$?; $G sparse-checkout list"),
        ("dashdash", "$G sparse-checkout set -- --cone; echo rc=$?; $G sparse-checkout list"),
        ("end-of-options", "$G sparse-checkout set --end-of-options a; echo rc=$?; $G sparse-checkout list"),
    ] {
        assert_twin(tag, CONE, false, scenario);
    }
    assert_twin(
        "stdin-empty-lines",
        NO_CONE,
        false,
        "printf 'b\\n\\n\"c/d\"\\n' | $G sparse-checkout set --stdin; echo rc=$?; $G sparse-checkout list",
    );
}

#[test]
fn a_pattern_file_cone_mode_cannot_read() {
    assert_twin("init-cone", NO_CONE, false, "$G sparse-checkout init --cone; echo rc=$?; $G sparse-checkout list");
    assert_twin("reapply-cone", NO_CONE, false, "$G sparse-checkout reapply --cone; echo rc=$?");
    assert_twin(
        "add-refused",
        CONE,
        false,
        "printf 'a\\nb\\n' > .git/info/sparse-checkout; $G sparse-checkout list; $G sparse-checkout add c; echo rc=$?",
    );
    assert_twin(
        "negative-first",
        CONE,
        false,
        "printf '/*\\n!/*/\\n!/a/*/\\n/a/\\n' > .git/info/sparse-checkout; $G sparse-checkout list; $G sparse-checkout reapply; echo rc=$?",
    );
    assert_twin(
        "full-cone",
        CONE,
        false,
        "printf '/*\\n' > .git/info/sparse-checkout; $G sparse-checkout list; $G sparse-checkout reapply; echo rc=$?",
    );
    assert_twin(
        "comments",
        NO_CONE,
        false,
        "printf '# c\\n/a/\\n\\n/top \\n' > .git/info/sparse-checkout; $G sparse-checkout list; $G sparse-checkout add b; echo rc=$?",
    );
    assert_twin(
        "ignore-case",
        "\"$G\" config core.ignoreCase true; \"$G\" sparse-checkout set a",
        false,
        "printf '/*\\n!/*/\\n/A/\\n' > .git/info/sparse-checkout; $G sparse-checkout list; $G sparse-checkout reapply; echo rc=$?",
    );
}

#[test]
fn check_rules_lines() {
    assert_twin("rules-empty", CONE, false, "printf 'a/f\\r\\n\\ntop\\n' | $G sparse-checkout check-rules; echo rc=$?");
    assert_twin(
        "rules-z",
        CONE,
        false,
        "printf 'b\\0c\\0' > ../rules; printf 'a/f\\0b/f\\0' | $G sparse-checkout check-rules -z --rules-file ../rules | tr '\\0' '|'; echo rc=$?",
    );
    assert_twin(
        "rules-normalize",
        CONE,
        false,
        "printf ' b \\n../x\\n' > ../rules; echo b/f | $G sparse-checkout check-rules --rules-file ../rules; echo rc=$?",
    );
}
