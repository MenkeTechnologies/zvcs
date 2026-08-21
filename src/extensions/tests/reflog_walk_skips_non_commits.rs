//! `git reflog` is `git log --walk-reflogs`, and that walk hands out **commits**.
//!
//! An entry whose *new* object is not a commit is stepped over rather than
//! rendered. `next_reflog_entry()` only ever returns what `next_reflog_commit()`
//! found, and that is:
//!
//! ```c
//! for (; log->recno >= 0; log->recno--) {
//!         struct reflog_info *entry = &log->reflogs->items[log->recno];
//!         struct object *obj = parse_object(the_repository,
//!                                           &entry->noid);
//!
//!         if (obj && obj->type == OBJ_COMMIT)
//!                 return (struct commit *)obj;
//! }
//! return NULL;
//! ```
//! (`reflog-walk.c:341-352`)
//!
//! Two properties follow, and this file pins both:
//!
//! 1. **The test is `parse_object()` plus `type == OBJ_COMMIT`** — not "the new id
//!    is null", and not a peel. Four unrelated shapes fail it: the zero id a
//!    deletion records, an id whose object the repository does not hold, and ids
//!    that parse to a tree, a blob or an annotated tag.
//! 2. **A skipped entry still spends its `@{…}` number.** The selector is
//!    `strbuf_addf(sb, "%d", commit_reflog->reflogs->nr - 2 - commit_reflog->recno)`
//!    (`reflog-walk.c:266-267`) against a `recno` that `next_reflog_entry()` has
//!    already decremented past the entry it returned (`reflog-walk.c:379`), so it
//!    reads back as `nr - 1 - <array index>`: the survivor's *raw* position in the
//!    log, never a renumbering of the survivors.
//!
//! `branch -m` is the everyday source of case 1: a rename logs the old name's
//! deletion into `.git/logs/HEAD` (`<commit> -> 0{40}`) right next to the new
//! name's creation, so a rename-and-rename-back leaves `HEAD@{1}` and `HEAD@{3}`
//! unwalkable and stock prints `@{0}`, `@{2}`, `@{4}`.
//!
//! Every expectation below was captured from stock git 2.55.0 in the same fixture.
//! The raw `.git/logs/HEAD` written by the two binaries is byte-identical here —
//! the divergence this file guards was only ever in the walk, never in the writer,
//! so [`rename_round_trip_writes_the_same_log_stock_does`] pins the raw file too.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A one-commit repository on `main`, with an isolated `HOME` so no ambient
    /// configuration can reach the run.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-reflogwalk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.write("f.txt", b"a\n");
        f.git(&["add", "f.txt"]);
        f.git(&["commit", "-q", "-m", "initial"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .env("LC_ALL", "C");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    /// Stdout of a command that is expected to succeed, as a `String`.
    fn out(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "`git {args:?}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn rev(&self, spec: &str) -> String {
        self.out(&["rev-parse", spec]).trim().to_string()
    }

    fn write(&self, path: &str, body: &[u8]) {
        std::fs::write(self.work.join(path), body).unwrap();
    }

    fn log_path(&self, refname: &str) -> PathBuf {
        self.work.join(".git/logs").join(refname)
    }

    /// The `<old> <new>` id pair of each raw log line, oldest first — the shape of
    /// the file on disk, with the identity and clock fields dropped.
    fn raw_pairs(&self, refname: &str) -> Vec<(String, String)> {
        let text = std::fs::read_to_string(self.log_path(refname)).unwrap();
        text.lines()
            .map(|line| {
                let mut f = line.split(' ');
                (f.next().unwrap().to_string(), f.next().unwrap().to_string())
            })
            .collect()
    }
}

/// The `<ref>@{<n>}` selectors `git reflog --format=%gd` printed, in order.
fn selectors(text: &str) -> Vec<&str> {
    text.lines().collect()
}

const NULL_OID: &str = "0000000000000000000000000000000000000000";

#[test]
fn rename_round_trip_writes_the_same_log_stock_does() {
    // The premise of the whole file: the writer is already right. Renaming a
    // branch and renaming it back leaves five entries, and the two in the middle
    // of each pair record the old name's *deletion* — a null new id. Stock git
    // 2.55.0 writes exactly this, so anything that "fixed" the divergence by not
    // writing these lines would be corrupting the log, not repairing the walk.
    let f = Fixture::new("raw");
    let head = f.rev("HEAD");
    f.git(&["branch", "-m", "main", "renamed"]);
    f.git(&["branch", "-m", "renamed", "main"]);

    assert_eq!(
        f.raw_pairs("HEAD"),
        vec![
            (NULL_OID.to_string(), head.clone()), // commit (initial)
            (head.clone(), NULL_OID.to_string()), // main deleted
            (NULL_OID.to_string(), head.clone()), // renamed created
            (head.clone(), NULL_OID.to_string()), // renamed deleted
            (NULL_OID.to_string(), head.clone()), // main created
        ],
        "`branch -m` must keep writing git's deletion/creation pair per rename"
    );
}

#[test]
fn a_rename_round_trip_prints_three_entries_at_zero_two_and_four() {
    // Captured from stock git 2.55.0 in this fixture:
    //
    //     $ git reflog --format='%gd %H %gs'
    //     HEAD@{0} <head> Branch: renamed refs/heads/renamed to refs/heads/main
    //     HEAD@{2} <head> Branch: renamed refs/heads/main to refs/heads/renamed
    //     HEAD@{4} <head> commit (initial): initial
    //
    // Two assertions in one: the null-id entries at raw positions 1 and 3 are
    // gone, and the three survivors keep the numbers they had in the file.
    let f = Fixture::new("roundtrip");
    let head = f.rev("HEAD");
    f.git(&["branch", "-m", "main", "renamed"]);
    f.git(&["branch", "-m", "renamed", "main"]);

    let expected = format!(
        "HEAD@{{0}} {head} Branch: renamed refs/heads/renamed to refs/heads/main\n\
         HEAD@{{2}} {head} Branch: renamed refs/heads/main to refs/heads/renamed\n\
         HEAD@{{4}} {head} commit (initial): initial\n"
    );
    assert_eq!(f.out(&["reflog", "--format=%gd %H %gs"]), expected);

    // `git log -g` is the same walk under its other name, and must agree line for
    // line — including the numbering.
    assert_eq!(f.out(&["log", "-g", "--format=%gd %H %gs"]), expected);
}

#[test]
fn the_default_oneline_listing_agrees_with_the_format_listing() {
    // The abbreviated default rendering must not reintroduce the skipped entries
    // through a different code path — and in particular must never print an
    // abbreviated null id, which is what a rendered deletion entry looks like.
    let f = Fixture::new("oneline");
    f.git(&["branch", "-m", "main", "renamed"]);
    f.git(&["branch", "-m", "renamed", "main"]);

    let text = f.out(&["reflog"]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "expected three surviving entries, got: {text}");
    assert!(lines[0].contains("HEAD@{0}:"), "{text}");
    assert!(lines[1].contains("HEAD@{2}:"), "{text}");
    assert!(lines[2].contains("HEAD@{4}:"), "{text}");
    assert!(
        !text.contains("0000000"),
        "a deletion entry was rendered with a null commit id: {text}"
    );
}

#[test]
fn a_date_selector_drops_the_same_entries_it_just_cannot_number_them() {
    // `--date=` switches the `@{…}` column to dates (`get_reflog_selector`'s
    // `SELECTOR_DATE`/`force_date` branch, `reflog-walk.c:261-264`), which hides
    // the numbering — but it is the same walk, so the same three entries survive.
    let f = Fixture::new("date");
    f.git(&["branch", "-m", "main", "renamed"]);
    f.git(&["branch", "-m", "renamed", "main"]);

    let text = f.out(&["reflog", "--date=unix"]);
    assert_eq!(text.lines().count(), 3, "{text}");
    assert!(!text.contains("HEAD@{0}"), "a date selector must not count: {text}");
}

#[test]
fn an_index_selector_starts_inside_the_log_and_keeps_the_raw_numbers() {
    // `commit_reflog->recno = reflogs->nr - recno - 1` (`reflog-walk.c:227`) only
    // moves the *start*; the skipping and the numbering are unchanged after it.
    // `HEAD@{2}` therefore begins at raw entry 2 and prints `@{2}`, `@{4}`.
    let f = Fixture::new("indexsel");
    f.git(&["branch", "-m", "main", "renamed"]);
    f.git(&["branch", "-m", "renamed", "main"]);

    assert_eq!(selectors(&f.out(&["reflog", "--format=%gd", "HEAD@{2}"])), ["HEAD@{2}", "HEAD@{4}"]);
    // Starting on an entry that cannot be walked skips forward to the next one
    // that can, and that one keeps its own number.
    assert_eq!(selectors(&f.out(&["reflog", "--format=%gd", "HEAD@{3}"])), ["HEAD@{4}"]);
}

#[test]
fn a_branch_log_is_unaffected_because_a_rename_moves_it_whole() {
    // The counterpart to the HEAD case, and the reason the defect hid: renaming a
    // branch renames its log file rather than appending a deletion to it, so
    // `refs/heads/main`'s log has no unwalkable entry and numbers consecutively.
    // A fix that filtered entries out of the list instead of skipping over them
    // would still pass here — which is why it is not the only test.
    let f = Fixture::new("branchlog");
    f.git(&["branch", "-m", "main", "renamed"]);
    f.git(&["branch", "-m", "renamed", "main"]);

    assert_eq!(
        selectors(&f.out(&["reflog", "--format=%gd", "main"])),
        ["main@{0}", "main@{1}", "main@{2}"]
    );
}

#[test]
fn every_non_commit_new_object_is_skipped_not_just_the_null_id() {
    // The discriminating case. `refs/zz/x` is walked through five real ref updates
    // — commit, annotated tag, tree, blob, commit — and then two hand-written
    // entries naming an object the repository does not hold. Stock git 2.55.0
    // prints exactly three lines for this log:
    //
    //     $ git reflog --format='%gd %gs' refs/zz/x
    //     refs/zz/x@{0}: back
    //     refs/zz/x@{2}:
    //     refs/zz/x@{6}:
    //
    // so the tag at raw 5, the tree at raw 4, the blob at raw 3 and the missing
    // object at raw 1 are all dropped for the one reason `parse_object() &&
    // type == OBJ_COMMIT` gives. The tag is the load-bearing one: it is a
    // non-null id whose object exists and peels to the very commit at `@{0}`, so
    // only a port that refuses to peel drops it the way git does.
    let f = Fixture::new("kinds");
    // `core.logAllRefUpdates=always` is what gives a ref outside refs/heads a log
    // at all.
    f.git(&["config", "core.logAllRefUpdates", "always"]);
    let commit = f.rev("HEAD");
    let tree = f.rev("HEAD^{tree}");
    let blob = f.rev("HEAD:f.txt");
    f.git(&["tag", "-a", "-m", "tagmsg", "atag", "HEAD"]);
    let tag = f.rev("atag");
    assert_ne!(tag, commit, "an annotated tag must be its own object");

    for id in [&commit, &tag, &tree, &blob, &commit] {
        f.git(&["update-ref", "refs/zz/x", id]);
    }

    // Two more entries whose new object is absent. `update-ref` will not write
    // these (it insists the object exists), so they are appended by hand — which
    // is also what a pruned or partially-cloned repository ends up with.
    let missing = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let mut log = std::fs::read_to_string(f.log_path("refs/zz/x")).unwrap();
    log.push_str(&format!("{commit} {missing} t <t@e.co> 1700000000 +0000\tfabricated\n"));
    log.push_str(&format!("{missing} {commit} t <t@e.co> 1700000001 +0000\tback\n"));
    std::fs::write(f.log_path("refs/zz/x"), log).unwrap();

    assert_eq!(f.raw_pairs("refs/zz/x").len(), 7, "the fixture must have seven raw entries");
    assert_eq!(
        f.out(&["reflog", "--format=%gD %H %gs", "refs/zz/x"]),
        format!(
            "refs/zz/x@{{0}} {commit} back\n\
             refs/zz/x@{{2}} {commit} \n\
             refs/zz/x@{{6}} {commit} \n"
        )
    );
}
