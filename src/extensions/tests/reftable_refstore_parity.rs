//! `zvcs::refstore`, the reflog and root-ref helpers the porcelain uses instead
//! of touching ref storage files: in a `files` repository they read and write
//! exactly the files the porcelain always did, in a `reftable` repository they
//! go through the backend, and both answer what stock git answers.
//!
//! Every repository is built by stock git. The reftable twin of a files
//! repository is made by stock `refs migrate --ref-format=reftable`, which
//! copies every reflog entry, so the two must yield identical entries.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gix::bstr::{BString, ByteSlice};
use zvcs::refstore::{self, ReflogEntry, StateRef};

struct Fixture {
    root: PathBuf,
    stock: &'static str,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 46, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-refstore-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Some(Fixture { root, stock })
    }

    fn stock(&self, args: &[&str]) -> Output {
        Command::new(self.stock)
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_TEST_REFTABLE_AUTOCOMPACTION")
            .env("LC_ALL", "C")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .output()
            .unwrap()
    }

    fn run(&self, args: &[&str]) -> String {
        let out = self.stock(args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn path(&self, dir: &str) -> PathBuf {
        self.root.join(dir)
    }

    /// `F`, a files repository with reflogs on `HEAD` and several branches,
    /// including names whose order differs between a per-directory sort and a
    /// `strcmp()` of the whole name (`a-b` and `a/c`), and `T`, its reftable twin.
    fn files_and_reftable_twin(&self) {
        for args in [
            &["init", "-q", "-b", "main", "F"][..],
            &["-C", "F", "commit", "-q", "--allow-empty", "-m", "one"],
            &["-C", "F", "commit", "-q", "--allow-empty", "-m", "two"],
            &["-C", "F", "branch", "a-b"],
            &["-C", "F", "branch", "a/c", "HEAD~"],
            &["-C", "F", "checkout", "-q", "-b", "topic"],
            &["-C", "F", "commit", "-q", "--allow-empty", "-m", "three"],
            &["-C", "F", "reset", "-q", "--hard", "HEAD~"],
            &["-C", "F", "checkout", "-q", "main"],
        ] {
            self.run(args);
        }
        let status = Command::new("cp").arg("-R").arg(self.path("F")).arg(self.path("T")).status().unwrap();
        assert!(status.success());
        self.run(&["-C", "T", "refs", "migrate", "--ref-format=reftable"]);
    }

    /// `<name>`, a repository of format `format` with the linked worktree
    /// `<name>-wt` on `side`, each with reflog entries of its own.
    fn with_worktree(&self, name: &str, format: &str) {
        let wt = format!("{name}-wt");
        for args in [
            &["init", "-q", "-b", "main", &format!("--ref-format={format}"), name][..],
            &["-C", name, "commit", "-q", "--allow-empty", "-m", "one"],
            &["-C", name, "branch", "side"],
            &["-C", name, "worktree", "add", "-q", &format!("../{wt}"), "side"],
            &["-C", &wt, "commit", "-q", "--allow-empty", "-m", "wt"],
            &["-C", &wt, "update-ref", "-m", "bisect", "--create-reflog", "refs/bisect/bad", "HEAD"],
        ] {
            self.run(args);
        }
    }
}

fn open(dir: &Path) -> gix::Repository {
    gix::open_opts(dir, gix::open::Options::isolated()).unwrap()
}

fn lines(text: &str) -> Vec<BString> {
    text.lines().map(BString::from).collect()
}

fn entries(repo: &gix::Repository, name: &str, reverse: bool) -> (bool, Vec<ReflogEntry>) {
    let mut out = Vec::new();
    let found = refstore::for_each_reflog_entry(repo, name, reverse, |e| {
        out.push(e.clone());
        ControlFlow::Continue(())
    })
    .unwrap();
    (found, out)
}

#[test]
fn reflog_names_match_stock_reflog_list_in_both_formats() {
    let Some(f) = Fixture::new("names") else { return };
    f.files_and_reftable_twin();
    for dir in ["F", "T"] {
        let repo = open(&f.path(dir));
        assert_eq!(
            refstore::reflog_names(&repo, false).unwrap(),
            lines(&f.run(&["-C", dir, "reflog", "list"])),
            "{dir}: the order is git's, a/c before a-b in files as in reftable"
        );
    }
    assert!(refstore::is_reftable(&open(&f.path("T"))));
    assert!(!refstore::is_reftable(&open(&f.path("F"))));
}

#[test]
fn reflog_entries_are_identical_across_formats_and_match_stock() {
    let Some(f) = Fixture::new("entries") else { return };
    f.files_and_reftable_twin();
    let files = open(&f.path("F"));
    let reftable = open(&f.path("T"));
    for name in ["HEAD", "refs/heads/main", "refs/heads/topic", "refs/heads/a/c"] {
        let (found, forward) = entries(&files, name, false);
        assert!(found, "{name} has a reflog file");
        assert_eq!(entries(&reftable, name, false), (true, forward.clone()), "{name}: forward");
        let (_, mut reverse) = entries(&files, name, true);
        assert_eq!(entries(&reftable, name, true).1, reverse, "{name}: reverse");
        reverse.reverse();
        assert_eq!(reverse, forward, "{name}: reverse is forward backwards");

        // Newest first, as stock shows them.
        let shown = lines(&f.run(&["-C", "F", "log", "-g", "--format=%gs%x09%gn <%ge>%x09%H", name]));
        let ours: Vec<BString> = forward
            .iter()
            .rev()
            .map(|e| {
                let msg = e.message.strip_suffix(b"\n").unwrap_or(&e.message);
                format!("{}\t{}\t{}", msg.as_bstr(), e.committer, e.new_oid).into()
            })
            .collect();
        assert_eq!(ours, shown, "{name}");
        assert!(forward.iter().all(|e| e.timestamp == 1112911993 && e.tz == -700));
        assert!(forward.iter().all(|e| e.message.ends_with(b"\n")), "messages keep their LF");
    }

    let (found, none) = entries(&files, "refs/heads/nope", false);
    assert!(!found && none.is_empty(), "files: a missing log is git's -1");
    let (found, none) = entries(&reftable, "refs/heads/nope", false);
    assert!(found && none.is_empty(), "reftable: git returns 0 without entries");

    for repo in [&files, &reftable] {
        assert!(refstore::reflog_exists(repo, "refs/heads/a/c"));
        assert!(refstore::reflog_exists(repo, "HEAD"));
        assert!(!refstore::reflog_exists(repo, "refs/heads/nope"));
    }

    // Stopping early stops the walk.
    let mut seen = 0;
    refstore::for_each_reflog_entry(&reftable, "HEAD", true, |_| {
        seen += 1;
        ControlFlow::Break(())
    })
    .unwrap();
    assert_eq!(seen, 1);
}

#[test]
fn files_state_refs_are_the_bytes_the_porcelain_writes() {
    let Some(f) = Fixture::new("files-state") else { return };
    f.files_and_reftable_twin();
    let repo = open(&f.path("F"));
    let git_dir = f.path("F/.git");
    let head: gix::ObjectId = f.run(&["-C", "F", "rev-parse", "HEAD"]).trim().parse().unwrap();

    assert!(!refstore::state_ref_exists(&repo, "CHERRY_PICK_HEAD"));
    assert_eq!(refstore::state_ref_read(&repo, "CHERRY_PICK_HEAD").unwrap(), None);
    refstore::state_ref_write(&repo, "CHERRY_PICK_HEAD", &StateRef::Object(head), "").unwrap();
    assert_eq!(std::fs::read(git_dir.join("CHERRY_PICK_HEAD")).unwrap(), format!("{head}\n").as_bytes());
    assert!(!git_dir.join("logs/CHERRY_PICK_HEAD").exists(), "no reflog is written");
    assert!(refstore::state_ref_exists(&repo, "CHERRY_PICK_HEAD"));
    assert_eq!(
        refstore::state_ref_read(&repo, "CHERRY_PICK_HEAD").unwrap(),
        Some(StateRef::Object(head))
    );
    assert_eq!(f.run(&["-C", "F", "rev-parse", "--verify", "CHERRY_PICK_HEAD"]).trim(), head.to_string());

    refstore::state_ref_write(
        &repo,
        "NOTES_MERGE_REF",
        &StateRef::Symbolic("refs/notes/commits".into()),
        "",
    )
    .unwrap();
    assert_eq!(
        std::fs::read(git_dir.join("NOTES_MERGE_REF")).unwrap(),
        b"ref: refs/notes/commits\n"
    );
    assert_eq!(
        refstore::state_ref_read(&repo, "NOTES_MERGE_REF").unwrap(),
        Some(StateRef::Symbolic("refs/notes/commits".into()))
    );

    // `read_ref_internal()` trims the file before parsing; FETCH_HEAD-style
    // trailing data after the id is accepted, garbage is a broken ref.
    std::fs::write(git_dir.join("REBASE_HEAD"), format!("{head}\t\tnot-for-merge\n")).unwrap();
    assert_eq!(refstore::state_ref_read(&repo, "REBASE_HEAD").unwrap(), Some(StateRef::Object(head)));
    std::fs::write(git_dir.join("REBASE_HEAD"), format!("{head}x\n")).unwrap();
    assert_eq!(refstore::state_ref_read(&repo, "REBASE_HEAD").unwrap(), None);
    assert!(refstore::state_ref_exists(&repo, "REBASE_HEAD"), "the file exists");

    refstore::state_ref_delete(&repo, "CHERRY_PICK_HEAD", "").unwrap();
    refstore::state_ref_delete(&repo, "CHERRY_PICK_HEAD", "").unwrap();
    assert!(!git_dir.join("CHERRY_PICK_HEAD").exists());
}

#[test]
fn reftable_state_refs_go_through_the_backend() {
    let Some(f) = Fixture::new("reftable-state") else { return };
    f.files_and_reftable_twin();
    let repo = open(&f.path("T"));
    let git_dir = f.path("T/.git");
    let head: gix::ObjectId = f.run(&["-C", "T", "rev-parse", "HEAD"]).trim().parse().unwrap();

    refstore::state_ref_write(&repo, "CHERRY_PICK_HEAD", &StateRef::Object(head), "").unwrap();
    assert!(!git_dir.join("CHERRY_PICK_HEAD").exists(), "no file next to the stack");
    assert_eq!(f.run(&["-C", "T", "rev-parse", "--verify", "CHERRY_PICK_HEAD"]).trim(), head.to_string());
    assert!(refstore::state_ref_exists(&repo, "CHERRY_PICK_HEAD"));
    refstore::state_ref_write(
        &repo,
        "NOTES_MERGE_REF",
        &StateRef::Symbolic("refs/notes/commits".into()),
        "",
    )
    .unwrap();
    assert_eq!(f.run(&["-C", "T", "symbolic-ref", "NOTES_MERGE_REF"]).trim(), "refs/notes/commits");
    f.run(&["-C", "T", "refs", "verify"]);
    assert_eq!(
        f.run(&["-C", "T", "for-each-ref", "--include-root-refs", "--format=%(refname)", "--exclude=refs/"]),
        "CHERRY_PICK_HEAD\nHEAD\nORIG_HEAD\n",
        "the root refs are in the stack; NOTES_MERGE_REF dangles, which iteration omits"
    );

    // What stock writes, read back.
    f.run(&["-C", "T", "update-ref", "--no-deref", "AUTO_MERGE", "HEAD~"]);
    let parent: gix::ObjectId = f.run(&["-C", "T", "rev-parse", "HEAD~"]).trim().parse().unwrap();
    assert_eq!(refstore::state_ref_read(&repo, "AUTO_MERGE").unwrap(), Some(StateRef::Object(parent)));

    refstore::state_ref_delete(&repo, "CHERRY_PICK_HEAD", "").unwrap();
    refstore::state_ref_delete(&repo, "CHERRY_PICK_HEAD", "").unwrap();
    assert!(!f.stock(&["-C", "T", "rev-parse", "--verify", "-q", "CHERRY_PICK_HEAD"]).status.success());
    assert_eq!(refstore::state_ref_read(&repo, "CHERRY_PICK_HEAD").unwrap(), None);
    f.run(&["-C", "T", "refs", "verify"]);
    f.run(&["-C", "T", "fsck", "--no-progress"]);

    // MERGE_HEAD stays a file whatever the format.
    refstore::state_ref_write(&repo, "MERGE_HEAD", &StateRef::Object(head), "").unwrap();
    assert_eq!(std::fs::read(git_dir.join("MERGE_HEAD")).unwrap(), format!("{head}\n").as_bytes());
    assert_eq!(refstore::state_ref_read(&repo, "MERGE_HEAD").unwrap(), Some(StateRef::Object(head)));
}

/// `add_reflogs_to_pending()`'s walk, derived from stock's `reflog list` in
/// each worktree: this store's names, then each other worktree's with its
/// per-worktree names prefixed.
fn expected_all(f: &Fixture, here: &str, others: &[(&str, &str)]) -> Vec<BString> {
    let mut out = lines(&f.run(&["-C", here, "reflog", "list"]));
    for (dir, prefix) in others {
        for name in lines(&f.run(&["-C", dir, "reflog", "list"])) {
            let per_worktree = !name.starts_with(b"refs/") || name.starts_with(b"refs/bisect/");
            out.push(if per_worktree { format!("{prefix}{name}").into() } else { name });
        }
    }
    out
}

#[test]
fn reflog_names_of_all_worktrees_in_both_formats() {
    let Some(f) = Fixture::new("worktrees") else { return };
    for (name, format) in [("files", "files"), ("rt", "reftable")] {
        f.with_worktree(name, format);
        let wt = format!("{name}-wt");
        let main = open(&f.path(name));
        let linked = open(&f.path(&wt));

        let wt_names = lines(&f.run(&["-C", &wt, "reflog", "list"]));
        assert_eq!(refstore::reflog_names(&linked, false).unwrap(), wt_names, "{format}: in the worktree");
        assert!(wt_names.contains(&"refs/bisect/bad".into()), "{format}: the worktree's own log");
        assert_eq!(
            refstore::reflog_names(&main, false).unwrap(),
            lines(&f.run(&["-C", name, "reflog", "list"])),
            "{format}: in the main worktree"
        );

        assert_eq!(
            refstore::reflog_names(&main, true).unwrap(),
            expected_all(&f, name, &[(&wt, "worktrees/wt/")]).into_iter().map(|n| {
                // The worktree is named after its directory.
                n.to_str().unwrap().replace("worktrees/wt/", &format!("worktrees/{wt}/")).into()
            }).collect::<Vec<BString>>(),
            "{format}: from the main worktree"
        );
        assert_eq!(
            refstore::reflog_names(&linked, true).unwrap(),
            expected_all(&f, &wt, &[(name, "main-worktree/")]),
            "{format}: from the linked worktree"
        );

        let (_, wt_head) = entries(&linked, "HEAD", false);
        let (_, via_main) = entries(&main, &format!("worktrees/{wt}/HEAD"), false);
        assert!(!wt_head.is_empty());
        assert_eq!(via_main, wt_head, "{format}: another worktree's log by its prefixed name");
    }
}

/// A reftable log record with an empty name reads back with the account's
/// login name, as `fmt_ident()` substitutes it (ident.c:509-518); stock's
/// `log -g` shows the same name. The record is made by migrating a files
/// reflog whose line has no name.
#[test]
fn an_empty_reflog_name_reads_as_the_login_name() {
    let Some(f) = Fixture::new("empty-name") else { return };
    f.run(&["init", "-q", "-b", "main", "F"]);
    f.run(&["-C", "F", "commit", "-q", "--allow-empty", "-m", "one"]);
    let log = f.path("F/.git/logs/refs/heads/main");
    let text = std::fs::read_to_string(&log).unwrap();
    std::fs::write(&log, text.replace(" C O Mitter <", " <")).unwrap();
    let status = Command::new("cp").arg("-R").arg(f.path("F")).arg(f.path("T")).status().unwrap();
    assert!(status.success());
    f.run(&["-C", "T", "refs", "migrate", "--ref-format=reftable"]);

    let shown = f.run(&["-C", "T", "log", "-g", "--format=%gn <%ge>", "main"]);
    let (_, reftable) = entries(&open(&f.path("T")), "refs/heads/main", false);
    assert_eq!(reftable.len(), 1);
    assert_eq!(reftable[0].committer, shown.trim_end(), "the login name, as stock shows it");
    assert!(!shown.starts_with(" <"), "stock substituted a name: {shown}");

    let (_, files) = entries(&open(&f.path("F")), "refs/heads/main", false);
    assert_eq!(files[0].committer, "<committer@example.com>", "files hands the line on as it is");
}
