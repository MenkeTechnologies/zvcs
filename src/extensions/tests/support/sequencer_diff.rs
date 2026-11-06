//! A differential harness for the sequencer verbs: build one fixture with stock
//! git, run the same shell steps under stock git and under zvcs in that fixture
//! (at the same path, one after the other, so paths printed by either agree),
//! and return both transcripts.
//!
//! A transcript holds every step's stdout, stderr and exit code, then the
//! repository as stock git reads it afterwards: the refs, `HEAD`, the porcelain
//! v2 status, the index, the state files the sequencer and `git commit` write
//! (`CHERRY_PICK_HEAD`, `REVERT_HEAD`, `AUTO_MERGE`, `MERGE_MSG`, `SQUASH_MSG`,
//! `COMMIT_EDITMSG`, …), every file under `.git/rebase-merge`, `.git/rebase-apply`
//! and `.git/sequencer`, the history of every ref and `HEAD`'s reflog.
//!
//! Identity, both dates and the time zone are pinned, so rewritten commit ids are
//! reproducible and a legitimate part of the comparison.
//!
//! Each file includes this with
//!
//! ```ignore
//! #[path = "support/sequencer_diff.rs"]
//! mod sequencer_diff;
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// The commands every fixture script may use: `c <file> <content> <subject>`
/// writes, stages and commits with a date one minute past the previous commit's.
const PRELUDE: &str = r##"set -e
n=0
tick() { n=$((n+1)); t=$((1700000000 + n * 60)); GIT_AUTHOR_DATE="@$t +0000"; GIT_COMMITTER_DATE="@$t +0000"; export GIT_AUTHOR_DATE GIT_COMMITTER_DATE; }
c() { printf '%b' "$2" >"$1"; git add "$1"; tick; git commit -q -m "$3"; }
git init -q -b main
"##;

/// Editor scripts the steps can name through `$ED/<name>`.
const EDITORS: &[(&str, &str)] = &[
    // Shows the message it was handed on stderr and leaves it as it is.
    ("msg-dump", "cat \"$1\" >&2"),
    // Appends to the first line, which may be a comment line.
    ("msg-rw", "perl -i -pe 's/^(.+)$/$1 RW/ if $.==1' \"$1\""),
    // `fixup -c` / `fixup -C` / `squash` / `reword` / `edit` of the commit
    // whose subject is t3 or t2, and a sheet the parser rejects.
    ("seq-fixupc3", "perl -i -pe 's/^pick (\\S+) # t3$/fixup -c $1 t3/' \"$1\""),
    ("seq-fixup-replace3", "perl -i -pe 's/^pick (\\S+) # t3$/fixup -C $1 t3/' \"$1\""),
    ("seq-squash3", "perl -i -pe 's/^pick (\\S+) # t3$/squash $1 t3/' \"$1\""),
    ("seq-reword2", "perl -i -pe 's/^pick (\\S+) # (t2|p2)$/reword $1 $2/' \"$1\""),
    ("seq-edit2", "perl -i -pe 's/^pick (\\S+) # (t2|p2)$/edit $1 $2/' \"$1\""),
    ("seq-bogus", "perl -i -pe 's/^pick (\\S+) # t2$/bogus $1 t2/' \"$1\""),
    ("seq-break1", "perl -i -pe 's/^(pick \\S+ # t1)$/$1\\nbreak/' \"$1\""),
    ("seq-execdirty", "perl -i -pe 's/^(pick \\S+ # t1)$/$1\\nexec echo dirty >>t1.txt/' \"$1\""),
];

/// The stdout/stderr/exit code of every step and the repository afterwards, as
/// one string per implementation.
pub struct Transcripts {
    pub stock: String,
    pub zvcs: String,
}

/// Run `steps` (each one `sh -c` line, `git` on `PATH` being the binary under
/// test) against the fixture `script` builds, once under `stock` and once under
/// zvcs.
pub fn run(stock: &str, tag: &str, script: &str, steps: &[&str]) -> Transcripts {
    let root = std::env::temp_dir().join(format!("zvcs-seqdiff-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(root.join("home/.gitconfig"), "").unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let ed = root.join("ed");
    std::fs::create_dir_all(&ed).unwrap();
    for (name, body) in EDITORS {
        write_script(&ed.join(name), body);
    }
    let stock_bin = bin_dir(&root, "bin-stock", stock);
    let zvcs_bin = bin_dir(&root, "bin-zvcs", ZVCS);
    let mut out = Vec::new();
    for bin in [&stock_bin, &zvcs_bin] {
        let repo = root.join("repo");
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let built = sh(&root, &repo, &stock_bin, &format!("{PRELUDE}{script}"));
        assert!(built.status.success(), "fixture: {}", String::from_utf8_lossy(&built.stderr));
        let mut transcript = String::new();
        for (i, step) in steps.iter().enumerate() {
            let o = sh(&root, &repo, bin, step);
            transcript.push_str(&format!(
                "== step {}: {step}\n-- out\n{}-- err\n{}-- rc {:?}\n",
                i + 1,
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr),
                o.status.code()
            ));
        }
        transcript.push_str(&post_state(&root, &repo, &stock_bin));
        out.push(transcript);
    }
    let _ = std::fs::remove_dir_all(&root);
    let zvcs = out.pop().unwrap();
    let stock = out.pop().unwrap();
    Transcripts { stock, zvcs }
}

/// Assert both transcripts agree, showing both in full when they do not.
pub fn assert_same(t: &Transcripts) {
    assert!(
        t.stock == t.zvcs,
        "stock and zvcs differ\n--- stock\n{}\n--- zvcs\n{}",
        t.stock,
        t.zvcs
    );
}

fn post_state(root: &Path, repo: &Path, stock_bin: &Path) -> String {
    let script = r###"
echo "== post"
git for-each-ref --format='%(refname) %(objectname)'
echo "HEAD $(git rev-parse HEAD 2>&1) $(git symbolic-ref -q HEAD)"
git status --porcelain=v2 2>&1
git ls-files -s 2>&1
for f in CHERRY_PICK_HEAD REVERT_HEAD AUTO_MERGE ORIG_HEAD MERGE_MSG SQUASH_MSG REBASE_HEAD MERGE_HEAD COMMIT_EDITMSG; do
  if [ -e .git/$f ]; then echo "## $f"; cat .git/$f; fi
done
for d in rebase-merge rebase-apply sequencer; do
  if [ -d .git/$d ]; then
    find .git/$d -type f | LC_ALL=C sort | while read f; do echo "## $f"; cat "$f"; done
  fi
done
echo "## log"; git log --format='%H %P %an %ad %cn %s%n%b' --date=raw --all 2>&1
echo "## reflog"; git reflog -n 15 --format='%H %gs' 2>&1
"###;
    let o = sh(root, repo, stock_bin, script);
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn sh(root: &Path, repo: &Path, bin: &Path, script: &str) -> std::process::Output {
    Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(repo)
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", root.join("home"))
        .env("ED", root.join("ed"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "@1800000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1800000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("TERM", "dumb")
        .output()
        .unwrap()
}

/// A directory holding one `git`, a link to `target`, for the front of `PATH`.
fn bin_dir(root: &Path, name: &str, target: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(target, dir.join("git")).unwrap();
    dir
}

fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
