//! Updating a tag the repository already has.
//!
//! ```c
//! if (!is_null_oid(&ref->old_oid) &&
//!     starts_with(ref->name, "refs/tags/")) {
//!         if (force || ref->force) {
//!                 r = s_update_ref("updating tag", ref, transaction, 0);
//!                 info = ref_update_display_info_append(display_array, 't', '!',
//!                                                       _("[tag update]"), ...
//!         } else {
//!                 info = ref_update_display_info_append(display_array, '!', '!',
//!                                                       _("[rejected]"), NULL,
//!                                                       _("would clobber existing tag"), ...
//! ```
//!
//! (`update_local_ref()`, builtin/fetch.c:980-1006.) Three rules come out of
//! that block, all measured against stock git 2.55.0 over a local path:
//!
//! * `force` is the *command-line* `--force`, read before the refspec's own `+`.
//!   `--tags` fetches through `TAG_REFSPEC`, which carries no `+`
//!   (builtin/fetch.c:582-588), so `--tags --force` has to update the tag even
//!   though the refspec alone would not.
//! * The row for a tag that already existed is `t [tag update]` — never a range
//!   and never `(forced update)`, because this branch sits above the
//!   fast-forward test.
//! * Without force the update is refused, the tag keeps its old value and the
//!   command exits 1. That refusal is owed for *any* refspec that maps the tag:
//!   one written on the command line, `--tags`, or the whole-namespace spec `-P`
//!   brings in. Only automatic tag following says nothing, and only because
//!   `find_non_local_tags()` never proposes a tag that is already here.
//!
//! No network: the remote is a directory beside the clone.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn zvcs(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("run zvcs git")
}

fn err_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let out = zvcs(dir, home, args);
    assert!(out.status.success(), "{args:?} failed: {}", err_text(&out));
    out
}

fn rev(dir: &Path, home: &Path, name: &str) -> String {
    let out = ok(dir, home, &["rev-parse", name]);
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// A remote with a lightweight tag `lw` and an annotated tag `ann`, both moved
/// to a second commit *after* the clone was taken — so the clone holds the old
/// tags and the objects the new ones point at.
///
/// Returns the scratch root, the home directory, the clone factory's source, and
/// the two commits.
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    first: String,
    second: String,
    clones: std::cell::Cell<u32>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-tagclobber-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("mkdir home");
        let origin = root.join("origin");

        ok(&root, &home, &["init", "-q", "-b", "main", origin.to_str().expect("utf-8")]);
        ok(&origin, &home, &["commit", "--allow-empty", "-q", "-m", "c0"]);
        let first = rev(&origin, &home, "HEAD");
        ok(&origin, &home, &["tag", "lw"]);
        ok(&origin, &home, &["tag", "-a", "ann", "-m", "ann"]);
        ok(&origin, &home, &["commit", "--allow-empty", "-q", "-m", "c1"]);
        let second = rev(&origin, &home, "HEAD");
        Fixture { root, home, first, second, clones: std::cell::Cell::new(0) }
    }

    /// A clone taken before the tags move. Each call gets its own directory so
    /// the cases cannot see each other's writes.
    fn clone(&self) -> PathBuf {
        let n = self.clones.get();
        self.clones.set(n + 1);
        let dir = self.root.join(format!("work{n}"));
        ok(&self.root, &self.home, &["clone", "-q", "./origin", dir.to_str().expect("utf-8")]);
        dir
    }

    /// Move both tags onto the second commit, after every clone has been taken.
    fn move_tags(&self) {
        let origin = self.root.join("origin");
        ok(&origin, &self.home, &["tag", "-f", "lw", &self.second]);
        ok(&origin, &self.home, &["tag", "-f", "-a", "ann", "-m", "ann2", &self.second]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn tags_force_updates_the_tag_and_says_tag_update() {
    let fx = Fixture::new("tags-force");
    let work = fx.clone();
    fx.move_tags();
    assert_eq!(rev(&work, &fx.home, "lw"), fx.first, "the clone starts on the old tag");

    let out = zvcs(&work, &fx.home, &["fetch", "--tags", "--force", "origin"]);
    assert!(out.status.success(), "fetch --tags --force: {}", err_text(&out));
    let err = err_text(&out);
    assert!(
        err.contains(" t [tag update]      lw         -> lw\n"),
        "the lightweight tag reports `t [tag update]`:\n{err}"
    );
    assert!(
        err.contains(" t [tag update]      ann        -> ann\n"),
        "and so does the annotated one:\n{err}"
    );
    // The branch above the fast-forward test means no range and no trailer.
    assert!(!err.contains("(forced update)"), "no forced-update trailer for a tag:\n{err}");
    assert!(!err.contains(".."), "no range for a tag:\n{err}");
    assert_eq!(rev(&work, &fx.home, "lw"), fx.second, "the tag really moved");
}

#[test]
fn an_explicit_tag_refspec_is_refused_without_force() {
    let fx = Fixture::new("explicit");
    // The three unforced spellings that still map the tag, each in its own clone.
    // A `refs/tags/*:refs/tags/*` written by hand is spelled exactly like the
    // auto-follow refspec, which is what used to get it mistaken for one.
    let cases: [&[&str]; 3] = [
        &["fetch", "origin", "refs/tags/*:refs/tags/*"],
        &["fetch", "--tags", "origin"],
        &["fetch", "--prune", "--prune-tags", "origin"],
    ];
    // Every clone is taken before the tags move, so each holds the old ones.
    let works: Vec<PathBuf> = cases.iter().map(|_| fx.clone()).collect();
    fx.move_tags();

    for (args, work) in cases.iter().zip(works) {
        let out = zvcs(&work, &fx.home, args);
        let err = err_text(&out);
        assert!(!out.status.success(), "{args:?} must fail:\n{err}");
        assert!(
            err.contains(" ! [rejected] lw         -> lw  (would clobber existing tag)\n"),
            "{args:?} reports the refusal:\n{err}"
        );
        assert_eq!(rev(&work, &fx.home, "lw"), fx.first, "{args:?} left the tag alone");
    }
}

#[test]
fn a_forced_refspec_updates_the_tag_on_its_own() {
    let fx = Fixture::new("plus");
    let work = fx.clone();
    fx.move_tags();

    // `ref->force` — the refspec's own `+`, the other half of `force || ref->force`.
    let out = zvcs(&work, &fx.home, &["fetch", "origin", "+refs/tags/*:refs/tags/*"]);
    assert!(out.status.success(), "forced refspec: {}", err_text(&out));
    assert!(
        err_text(&out).contains(" t [tag update]      lw         -> lw\n"),
        "still `t [tag update]`, not a forced-update range:\n{}",
        err_text(&out)
    );
    assert_eq!(rev(&work, &fx.home, "lw"), fx.second, "the tag moved");
}

#[test]
fn automatic_tag_following_stays_silent_about_a_moved_tag() {
    let fx = Fixture::new("auto");
    let work = fx.clone();
    fx.move_tags();

    // No tag refspec anywhere: `find_non_local_tags()` asks the local ref store
    // first and offers only what is missing, so a tag that moved is not an
    // update at all — no row, exit 0, tag untouched.
    let out = zvcs(&work, &fx.home, &["fetch", "origin"]);
    assert!(out.status.success(), "plain fetch: {}", err_text(&out));
    let err = err_text(&out);
    assert!(!err.contains("lw"), "nothing said about the moved tag:\n{err}");
    assert!(!err.contains("ann"), "nor about the annotated one:\n{err}");
    assert_eq!(rev(&work, &fx.home, "lw"), fx.first, "and it is left where it was");
}
