//! `git remote` administration against stock git, each scenario run on twin
//! fixtures built by stock git and compared on stdout, stderr, exit code and the
//! state left behind: refs, reflogs, the config file and the legacy files.
//!
//! - `add`: `--mirror` with `-m` dies before anything is written
//!   (builtin/remote.c:208-209), a fetch mirror maps each `-t` branch to
//!   `+refs/<b>:refs/<b>` (`add_branch()`, :126-138), `remote.<n>.mirror` is written
//!   before `tagOpt` (:243-254), and an unknown `--mirror=` value is a bare
//!   `error:` at 129 (`parse_mirror_opt()`, :144-159). `-f` leaves the reflog
//!   entry `fetch` writes for `refs/remotes/<n>/HEAD` (builtin/fetch.c:1802-1803).
//! - `rename`: references move, and `remote.pushDefault` follows, only when a
//!   fetch refspec maps into `refs/remotes/<old>/` (:910-912, :985-992); a reflog
//!   is carried over, with its `remote: renamed` entry, only for a ref that had one
//!   (`rename_one_reflog()`, :672-711), null to null for a symref; a
//!   `remote.pushDefault` in a global file draws `handle_push_default()`'s warning.
//! - `rm`, `set-url`, `set-branches` and `set-head` accept a remote from
//!   `remotes/` or `branches/` (`remote_get()`), `rm` then failing on the missing
//!   section (:1092-1096) and `set-branches` on the missing fetch key (:1774-1777).
//! - `set-url` edits values where they stand, refuses a second match with
//!   `<key> has multiple values` (config.c:2686-2689), and `--add --delete` dies.
//! - `set-head -a -d` is a usage error; `set-head -a`, `prune` and `show` of a
//!   remote nothing configures contact the name as a URL; `prune` takes several.
//! - `show` names `rebases interactively` and the merges form, and `-v`, `get-url`
//!   and `show` rewrite URLs through `url.<base>.insteadOf` / `pushInsteadOf`.
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

/// Run `script` under `sh` in `dir` with `$G` naming the git to use and
/// everything that could differ between runs pinned.
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

/// An upstream `up` (`main` two commits ahead of `dev`'s base, `dev`,
/// `feature/x`, tags), `up2` a bare copy with an `extra` branch, and `repo` a
/// clone of `up` with a tracking `dev` and a second remote `other`.
const FIXTURE: &str = r#"set -e
c() { "$G" -c maintenance.auto=false "$@"; }
mkdir home
printf '[maintenance]\n\tauto = false\n' > home/.gitconfig
c init -q -b main up
cd up
echo a >a; c add a; c commit -qm one
c tag v1; c tag -a -m ann v2; c branch dev; c branch feature/x
echo b >b; c add b; c commit -qm two
c checkout -q dev; echo d >d; c add d; c commit -qm dev1; c checkout -q main
cd ..
c clone -q up repo
c clone -q --bare up up2
(cd up2 && c branch extra main)
cd repo
c branch --track dev origin/dev -q
c remote add other ../up2
c fetch -q other
c config branch.main.pushRemote other
mkdir -p .git/remotes .git/branches
printf 'URL: ../up\nPull: refs/heads/main:refs/heads/fromup\nPush: refs/heads/main:refs/heads/main\n' > .git/remotes/foo
printf '../up#dev\n' > .git/branches/bar
"#;

/// What a scenario leaves behind, read with stock git.
const STATE: &str = r#"
echo "== refs"; "$S" for-each-ref --format='%(refname) %(objectname) %(symref)'
echo "== config"; cat .git/config
echo "== logs"; (cd .git/logs && find refs -type f | sort | while read f; do echo "-- $f"; cat "$f"; done)
echo "== legacy"; for f in .git/remotes/* .git/branches/*; do [ -f "$f" ] && { echo "-- $f"; cat "$f"; }; done
echo "== global"; cat "$HOME/.gitconfig"
"#;

fn run_twin(tag: &str, scenario: &str) -> Option<[(String, String, i32); 2]> {
    let stock = stock_git()?;
    let mut results = Vec::new();
    for (side, git) in [("stock", stock), ("zvcs", BIN)] {
        let root = Root(std::env::temp_dir().join(format!("zvcs-remote-sweep-{tag}-{side}-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&root.0);
        std::fs::create_dir_all(&root.0).unwrap();
        let built = sh(stock, &root.0, &root.0, FIXTURE);
        assert_eq!(built.2, 0, "fixture failed: {built:?}");
        let repo = root.0.join("repo");
        let ran = sh(git, &root.0, &repo, scenario);
        let state = sh(stock, &root.0, &repo, &format!("S=\"$G\"; {STATE}"));
        results.push((format!("{}{}", ran.0, state.0), ran.1, ran.2));
    }
    let zvcs = results.pop().unwrap();
    let stock = results.pop().unwrap();
    Some([stock, zvcs])
}

fn assert_twin(tag: &str, scenario: &str) {
    let Some([stock, zvcs]) = run_twin(tag, scenario) else { return };
    assert_eq!(zvcs, stock, "scenario `{tag}` differs from stock git:\n{scenario}");
}

#[test]
fn add_mirror_combinations() {
    assert_twin(
        "add-mirror",
        r#"$G remote add --mirror -m main m0 ../up; echo rc=$?
$G remote add --mirror=fetch -t main -t dev m1 ../up; echo rc=$?
$G remote add --mirror --no-tags m2 ../up; echo rc=$?
$G remote add --mirror=push --tags m3 ../up; echo rc=$?
$G remote add --mirror=bogus m4 ../up; echo rc=$?
$G remote add -f -t main m5 ../up; echo rc=$?"#,
    );
}

#[test]
fn rename_follows_refspecs_reflogs_and_push_default() {
    assert_twin(
        "rename-refspec",
        r#"$G config --unset-all remote.origin.fetch; $G config remote.pushDefault origin
$G remote rename origin neo; echo rc=$?"#,
    );
    assert_twin(
        "rename-reflogs",
        r#"$G remote set-head origin dev; $G remote rename origin neo; echo rc=$?
$G remote rename neo origin; echo rc=$?"#,
    );
    assert_twin(
        "rename-global-push-default",
        r#"$G config --global remote.pushDefault origin; $G remote rename origin neo; echo rc=$?
$G remote rm neo; echo rc=$?"#,
    );
}

#[test]
fn legacy_remotes_are_remotes_to_rm_set_url_and_set_branches() {
    assert_twin(
        "legacy",
        r#"$G remote rm foo; echo rc=$?
$G remote set-url bar ../x; echo rc=$?
$G remote set-branches foo dev; echo rc=$?
$G remote set-branches --add foo dev; echo rc=$?
$G remote set-head foo -d; echo rc=$?
$G remote show bar; echo rc=$?"#,
    );
}

#[test]
fn set_url_edits_values_where_they_stand() {
    assert_twin(
        "set-url",
        r#"$G remote set-url --add origin ../a; $G remote set-url --add origin ../b
$G remote set-url origin ../z; echo rc=$?
$G remote set-url origin ../z '\.\./a'; echo rc=$?
$G remote set-url --delete origin '\.\./[bz]'; echo rc=$?
$G remote set-url --add --delete origin x; echo rc=$?
$G remote set-url --push --add origin p1; $G remote set-url --push --add origin p2
$G remote set-url --push --delete origin 1; echo rc=$?
$G remote set-url --delete origin '!up'; echo rc=$?
$G remote -v"#,
    );
    assert_twin(
        "set-branches",
        r#"$G remote set-branches origin dev; echo rc=$?
$G remote set-branches other; echo rc=$?
$G remote set-branches --add other extra; echo rc=$?
$G remote add --mirror=push pm ../up; $G remote set-branches pm dev; echo rc=$?"#,
    );
}

#[test]
fn set_head_prune_and_show_of_unconfigured_names() {
    assert_twin(
        "set-head",
        r#"$G remote set-head origin -a -d; echo rc=$?
$G remote set-head nosuch -a; echo rc=$?
(cd ../up && "$G" branch -D feature/x -q)
$G remote prune --dry-run origin other; echo rc=$?
$G remote prune origin nosuch; echo rc=$?
$G remote prune nosuch origin; echo rc=$?"#,
    );
}

#[test]
fn show_rebase_modes_and_url_rewrites() {
    assert_twin(
        "show-rebase",
        r#"$G config branch.main.rebase merges; $G config branch.dev.rebase interactive
$G remote show origin; echo rc=$?
$G config branch.dev.rebase bogus; $G remote show -n origin; echo rc=$?"#,
    );
    assert_twin(
        "url-rewrites",
        r#"$G config url.../.insteadOf ZZ:; $G config url.PP.pushInsteadOf ZZ:
$G remote set-url origin ZZ:up
$G remote -v; $G remote get-url origin; $G remote get-url --push --all origin
$G remote show -n origin; echo rc=$?"#,
    );
}
