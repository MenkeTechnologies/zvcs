//! What a fetch does with the updates the ref store refuses, and when its
//! `[deleted]` rows come out:
//!
//! * without `--atomic` the updates are one `REF_TRANSACTION_ALLOW_FAILURE`
//!   transaction (builtin/fetch.c:2056-2063): a D/F conflict with a loose ref
//!   (`lock_raw_ref()`) or a packed one (refs/files-backend.c:3064-3082) refuses
//!   that ref alone, `ref_transaction_rejection_handler()` reports the first name
//!   conflict as the `git remote prune` hint and the others by their text
//!   (:1832-1872), and the row reads `(unable to update local ref)` with the
//!   summary column kept;
//! * with `--atomic` the whole transaction fails: the summary is printed as if
//!   it went through, then the store's text, no `FETCH_HEAD`, no
//!   `refs/remotes/<r>/HEAD`, and the exit status is the
//!   `enum ref_transaction_error` (:2101-2110, :2186-2200);
//! * `prune_refs()` prints its rows as soon as the stale refs are deleted or
//!   queued (:1493-1504), so they precede a `reference-transaction` hook's output
//!   and a hook refusing the `--atomic` transaction.
//!
//! Each case builds identical fixtures with stock git and runs the fetch with
//! stock and with zvcs, on the files and the reftable backend.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::Path;
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// Run `script` under `sh` in a fresh `root`, with `$S` the stock git that builds the
/// fixture and `$G` the git under test, and report its output with `root` spelled `<D>`.
fn run(stock: &str, git: &str, root: &Path, format: &str, script: &str) -> String {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    let out = Command::new("sh")
        .args(["-c", script])
        .current_dir(root)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "maintenance.auto")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("COLUMNS", "80")
        .env("LC_ALL", "C")
        .env("S", stock)
        .env("G", git)
        .env("F", format)
        .output()
        .unwrap();
    let text = format!(
        "{}--- stderr\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.replace(&root.display().to_string(), "<D>")
}

fn compare(name: &str, script: &str) {
    let Some(stock) = stock_git() else { return };
    for format in ["files", "reftable"] {
        let root = std::env::temp_dir().join(format!("zvcs-fetch-refusals-{name}-{format}-{}", std::process::id()));
        let want = run(stock, stock, &root, format, script);
        let got = run(stock, ZVCS, &root, format, script);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(got, want, "{name} ({format})");
    }
}

/// `w` holds `main`, `side/x`, `loose/y` and `other`; `f` has the objects and
/// `refs/remotes/origin/side` packed and `refs/remotes/origin/loose` loose.
const CONFLICTS: &str = r#"
$S init -q -b main --ref-format=$F w
$S -C w commit -q --allow-empty -m one
$S -C w branch side/x; $S -C w branch other; $S -C w branch loose/y
$S init -q -b main --ref-format=$F f; cd f
$S remote add origin ../w
$S fetch -q origin main; $S update-ref -d refs/remotes/origin/main
c=$($S -C ../w rev-parse main)
$S update-ref refs/remotes/origin/side $c; $S update-ref refs/remotes/origin/loose $c
$S pack-refs --all; $S update-ref refs/remotes/origin/loose $c
show() { echo "exit $?"; cat .git/FETCH_HEAD; $S for-each-ref; $S reflog list; }
"#;

#[test]
fn a_name_conflict_refuses_that_ref_alone() {
    compare("non-atomic", &format!("{CONFLICTS}$G fetch origin; show"));
    compare(
        "explicit",
        &format!("{CONFLICTS}$G fetch origin loose/y:refs/remotes/origin/loose/y main:refs/remotes/origin/m2; show"),
    );
}

#[test]
fn an_atomic_fetch_with_a_name_conflict_writes_nothing() {
    compare("atomic", &format!("{CONFLICTS}$G fetch --atomic origin; show"));
    compare(
        "atomic-packed",
        &format!("{CONFLICTS}$G fetch --atomic origin side/x:refs/remotes/origin/side/x; show"),
    );
}

#[test]
fn the_hook_sees_the_refused_refs_and_set_head_after_the_updates() {
    let hook = "printf '#!/bin/sh\\necho \"hook $1\" >&2\\ncat >&2\\n' > .git/hooks/reference-transaction\nchmod +x .git/hooks/reference-transaction\n";
    compare("hook", &format!("{CONFLICTS}{hook}$G fetch origin; show"));
    compare("hook-atomic", &format!("{CONFLICTS}{hook}$G fetch --atomic origin; show"));
}

/// `f` is a clone of `w` from which `gone1` and `gone2` have since been deleted.
const PRUNE: &str = r#"
$S init -q -b main --ref-format=$F w
$S -C w commit -q --allow-empty -m one; $S -C w branch gone1; $S -C w branch gone2
$S clone -q --ref-format=$F w f
$S -C w branch -D gone1 gone2 >/dev/null; $S -C w commit -q --allow-empty -m two; $S -C w branch newb
cd f
show() { echo "exit $?"; $S for-each-ref; }
"#;

#[test]
fn pruned_rows_come_out_before_the_fetch_goes_on() {
    for phase in ["preparing", "prepared"] {
        let hook = format!(
            "printf '#!/bin/sh\\necho \"hook $1\" >&2\\ncat >&2\\n[ \"$1\" = {phase} ] && exit 1\\nexit 0\\n' > .git/hooks/reference-transaction\nchmod +x .git/hooks/reference-transaction\n"
        );
        compare(&format!("prune-atomic-{phase}"), &format!("{PRUNE}{hook}$G fetch --prune --atomic origin; show"));
        compare(&format!("prune-{phase}"), &format!("{PRUNE}{hook}$G fetch --prune origin; show"));
    }
    compare("prune-porcelain", &format!("{PRUNE}$G fetch --prune --porcelain origin; show"));
    compare("prune-dry-run", &format!("{PRUNE}$G fetch --prune --dry-run -v origin; show"));
}
