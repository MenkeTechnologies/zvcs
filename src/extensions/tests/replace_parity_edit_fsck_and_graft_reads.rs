//! `git replace --edit` refusing a malformed object, `--convert-graft-file` on a
//! graft whose commit is missing, and `<rev>~<n>` walking a replaced history.
//!
//! * `import_object()` hashes the edited file with `INDEX_FORMAT_CHECK`
//!   (builtin/replace.c:304-316), so `index_mem()` runs a strict `fsck_buffer()`
//!   and dies with `refusing to create malformed object` after `error: object
//!   fails fsck: <id>: <text>` (object-file.c:1007-1016); nothing is replaced.
//! * `register_commit_graft()` gives every graft id a commit object
//!   (commit.c:209-218, :245), so once a commit has been parsed a missing graft
//!   commit is `Could not read <id>` before `could not parse <id>`
//!   (commit.c:641-645, object.c:217-227); before that it is the second alone.
//! * `get_nth_ancestor()` follows `parents->item` `<n>` times with no seen-set
//!   (object-name.c:873-877), so a replacement whose parent is the commit it
//!   replaces still has ancestors.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// Four commits `c1`..`c4` touching `f1`..`f4`.
const FIXTURE: &str = r#"
set -e
t=1700000000
c() { t=$((t+3600)); GIT_AUTHOR_DATE="@$t +0000" GIT_COMMITTER_DATE="@$t +0000" "$GIT" -c maintenance.auto=false commit -q -m "$1"; }
"$GIT" init -q -b main .
for i in 1 2 3 4; do echo $i > f$i; "$GIT" add f$i; c c$i; done
"#;

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Twin {
    fn new(stock: &str, label: &str) -> Twin {
        let root = std::env::temp_dir().join(format!("zvcs-replace-fsck-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for side in ["stock", "zvcs"] {
            let dir = root.join(side);
            std::fs::create_dir_all(&dir).unwrap();
            let out = env(&mut Command::new("sh"), &root)
                .args(["-c", FIXTURE])
                .env("GIT", stock)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "fixture: {}", String::from_utf8_lossy(&out.stderr));
        }
        Twin { root }
    }

    fn sh(&self, bin: &str, side: &str, script: &str) -> (String, String, i32) {
        let out = env(&mut Command::new("sh"), &self.root)
            .args(["-c", script])
            .env("GIT", bin)
            .env("STOCK", stock_git().unwrap())
            .current_dir(self.root.join(side))
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    /// The script's output and the refs and graft file it left, read back with
    /// stock git.
    fn same(&self, stock: &str, script: &str) {
        let state = r#"; echo "rc=$?"; "$STOCK" for-each-ref; cat .git/info/grafts 2>/dev/null; true"#;
        let script = format!("{script}{state}");
        assert_eq!(self.sh(ZVCS, "zvcs", &script), self.sh(stock, "stock", &script), "{script}");
    }
}

fn env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    cmd.env("HOME", root)
        .env("GIT_EDITOR", "true")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1800000000 +0000")
        .env("GIT_COMMITTER_DATE", "1800000000 +0000")
        .env("ZVCS_HOME", root.join("zvcs-home"))
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_NO_REPLACE_OBJECTS")
        .env_remove("XDG_CONFIG_HOME")
}

#[test]
fn edit_refuses_an_object_that_fails_fsck() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "edit");
    t.same(stock, r#"GIT_EDITOR='perl -i -pe "s/^tree.*/tree bad/"' "$GIT" replace --edit HEAD"#);
    t.same(stock, r#"GIT_EDITOR='perl -i -pe "s/^author.*\n//"' "$GIT" replace --edit HEAD~1"#);
    t.same(stock, r#"GIT_EDITOR='printf "" >' "$GIT" replace --edit HEAD"#);
    t.same(stock, r#"GIT_EDITOR='perl -i -pe "s/^tagger.*\n//"' "$GIT" replace --edit --raw HEAD~2"#);
    t.same(stock, r#"GIT_EDITOR='perl -i -pe "s/c4/C4/"' "$GIT" replace --edit HEAD; "$GIT" log -1 --format=%s"#);
}

#[test]
fn convert_graft_file_cannot_read_a_missing_graft_commit_once_grafts_are_loaded() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "graft");
    let missing = "1111111111111111111111111111111111111111";
    // A real commit first: its parse loads the graft file.
    t.same(
        stock,
        &format!(r#"printf '%s\n{missing}\n' "$("$STOCK" rev-parse HEAD~1)" > .git/info/grafts; "$GIT" replace --convert-graft-file"#),
    );
    // The missing commit first: nothing parsed yet.
    t.same(
        stock,
        &format!(r#"for r in $("$STOCK" for-each-ref --format='%(refname)' refs/replace); do "$STOCK" update-ref -d $r; done; printf '{missing}\n%s\n' "$("$STOCK" rev-parse HEAD~2)" > .git/info/grafts; "$GIT" replace --convert-graft-file"#),
    );
}

#[test]
fn nth_ancestor_follows_a_replacement_back_onto_itself() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new(stock, "cycle");
    // c3 is replaced by c2, and c1 by c2 as well: c1 under replacement has
    // parent c1.
    t.same(stock, r#""$STOCK" replace HEAD~1 HEAD~2; "$STOCK" replace HEAD~3 HEAD~2; for n in 1 2 3 4 5; do "$GIT" rev-parse HEAD~$n; done"#);
    t.same(stock, r#""$GIT" rev-parse HEAD~2^ HEAD~3~2; "$GIT" log --format=%s -4"#);
}
