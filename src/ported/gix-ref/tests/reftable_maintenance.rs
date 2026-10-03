//! Reflog maintenance, optimize, rename/copy and fsck of the reftable backend,
//! compared with stock git 2.56.
//!
//! Each test builds a reftable repository with stock git, copies it, runs the
//! stock command on one copy and the backend operation on the other, and
//! requires the stacks to end up with byte-identical tables; stock git then
//! verifies the copy the backend wrote (`refs verify`, `fsck`) and reads it
//! back (`reflog list`, `log -g --date=raw`) exactly as its own.

#[path = "../../../extensions/tests/support/stock_git.rs"]
mod stock_git;

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use gix_hash::ObjectId;
use gix_ref::{
    FullNameRef,
    bstr::BString,
    reftable::{Backend, ExpireFlags, ExpirePolicy, FsckReport, WriteConfig},
};

const COMMITTER_NAME: &str = "C O Mitter";
const COMMITTER_EMAIL: &str = "committer@example.com";
const DATE: &str = "1112911993 -0700";

struct Fixture {
    _tmp: gix_testtools::tempfile::TempDir,
    root: PathBuf,
    git: &'static str,
}

impl Fixture {
    /// A reftable repository `R` built by stock git: `main` with three
    /// commits, `side` at the last one, and an annotated tag `v1`.
    fn new() -> Option<Fixture> {
        let Some(git) = stock_git::stock_git_at_least((2, 56, 0)) else {
            eprintln!("skipped: no stock git 2.56");
            return None;
        };
        let tmp = gix_testtools::tempfile::Builder::new()
            .prefix("p4-reftable-maintenance-")
            .tempdir()
            .expect("temp dir");
        let root = tmp.path().to_owned();
        let f = Fixture { _tmp: tmp, root, git };
        f.ok(&f.root, &["init", "-q", "-b", "main", "--ref-format=reftable", "R"]);
        let r = f.root.join("R");
        for msg in ["one", "two", "three"] {
            f.ok(&r, &["commit", "-q", "--allow-empty", "-m", msg]);
        }
        f.ok(&r, &["branch", "side"]);
        f.ok(&r, &["tag", "-a", "v1", "-m", "t"]);
        Some(f)
    }

    fn cmd(&self, dir: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(self.git);
        cmd.args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", COMMITTER_NAME)
            .env("GIT_COMMITTER_EMAIL", COMMITTER_EMAIL)
            .env("GIT_AUTHOR_DATE", DATE)
            .env("GIT_COMMITTER_DATE", DATE);
        cmd
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        self.cmd(dir, args).output().expect("stock git runs")
    }

    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.run(dir, args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8")
    }

    /// Two copies of `R`: one for stock git, one for the backend.
    fn copies(&self, name: &str) -> (PathBuf, PathBuf) {
        let stock = self.root.join(format!("{name}-stock"));
        let ours = self.root.join(format!("{name}-ours"));
        for dst in [&stock, &ours] {
            copy_dir(&self.root.join("R"), dst);
        }
        (stock, ours)
    }

    /// `stock` and `ours` hold the same tables, and stock git verifies `ours`
    /// and reads its reflogs as those of `stock`.
    fn assert_same(&self, stock: &Path, ours: &Path) {
        assert_eq!(tables(ours), tables(stock), "tables differ");
        for args in [&["refs", "verify"][..], &["fsck", "--no-progress"]] {
            let out = self.run(ours, args);
            assert!(
                out.status.success() && out.stderr.is_empty(),
                "git {args:?} on our copy: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        for args in [
            &["reflog", "list"][..],
            &["log", "-g", "--all", "--date=raw", "--format=%H %gd %gn <%ge> %gs"],
            &["for-each-ref", "--include-root-refs"],
        ] {
            assert_eq!(self.ok(ours, args), self.ok(stock, args), "git {args:?}");
        }
    }
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("mkdir");
    for entry in std::fs::read_dir(src).expect("readdir") {
        let entry = entry.expect("entry");
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).expect("copy");
        }
    }
}

/// The tables of the main stack in order: each name without its random
/// suffix (`0x<min>-0x<max>`) and its bytes.
fn tables(repo: &Path) -> Vec<(String, Vec<u8>)> {
    let dir = repo.join(".git/reftable");
    std::fs::read_to_string(dir.join("tables.list"))
        .expect("tables.list")
        .lines()
        .map(|name| {
            let prefix = name.rsplit_once('-').expect("table name").0.to_owned();
            (prefix, std::fs::read(dir.join(name)).expect("table"))
        })
        .collect()
}

fn backend(repo: &Path, configure: impl Fn(&mut WriteConfig) + Send + Sync + 'static) -> Backend {
    let backend = Backend::open(&repo.join(".git"), None, gix_hash::Kind::Sha1);
    backend.check().expect("stack opens");
    backend.set_write_config_fn(std::sync::Arc::new(move || {
        let mut config = WriteConfig::default();
        configure(&mut config);
        config
    }));
    backend
}

fn name(name: &str) -> &FullNameRef {
    name.try_into().expect("valid ref name")
}

fn committer() -> gix_actor::SignatureRef<'static> {
    gix_actor::SignatureRef {
        name: COMMITTER_NAME.into(),
        email: COMMITTER_EMAIL.into(),
        time: DATE,
    }
}

/// Prunes the entries at the given positions, counted from the oldest, and
/// records what the backend told it.
#[derive(Default)]
struct Prune {
    positions: Vec<usize>,
    all: bool,
    seen: usize,
    prepared: Option<(BString, ObjectId)>,
    messages: Vec<BString>,
    cleaned_up: bool,
}

impl ExpirePolicy for Prune {
    fn prepare(&mut self, refname: &FullNameRef, oid: &gix_hash::oid) {
        self.prepared = Some((refname.as_bstr().to_owned(), oid.to_owned()));
    }

    fn should_prune(&mut self, entry: &gix_ref::log::Line) -> bool {
        self.seen += 1;
        self.messages.push(entry.message.clone());
        self.all || self.positions.contains(&self.seen)
    }

    fn cleanup(&mut self) {
        self.cleaned_up = true;
    }
}

fn rev_parse(f: &Fixture, repo: &Path, rev: &str) -> ObjectId {
    ObjectId::from_hex(f.ok(repo, &["rev-parse", rev]).trim().as_bytes()).expect("hex")
}

#[test]
fn reflog_expire_all_leaves_an_existence_marker() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("expire-all");
    f.ok(&stock, &["reflog", "expire", "--expire=all", "refs/heads/main"]);

    let mut policy = Prune {
        all: true,
        ..Default::default()
    };
    backend(&ours, |_| {})
        .reflog_expire(name("refs/heads/main"), ExpireFlags::default(), &mut policy)
        .expect("expire");
    assert_eq!(
        policy.prepared,
        Some(("refs/heads/main".into(), rev_parse(&f, &ours, "main")))
    );
    assert_eq!(
        policy.messages,
        ["commit (initial): one", "commit: two", "commit: three"],
        "oldest first, without the newline"
    );
    assert!(policy.cleaned_up);
    f.assert_same(&stock, &ours);
    assert!(
        f.run(&ours, &["reflog", "exists", "refs/heads/main"]).status.success(),
        "the emptied reflog still exists"
    );
    assert_eq!(f.ok(&ours, &["reflog", "show", "refs/heads/main"]), "");
}

/// Compaction rewrites the message of the existence marker, so only an
/// uncompacted table shows the one expiry writes.
#[test]
fn reflog_expire_all_without_compaction() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("expire-all-uncompacted");
    let out = f
        .cmd(&stock, &["reflog", "expire", "--expire=all", "refs/heads/main"])
        .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let mut policy = Prune {
        all: true,
        ..Default::default()
    };
    backend(&ours, |c| c.opts.disable_auto_compact = true)
        .reflog_expire(name("refs/heads/main"), ExpireFlags::default(), &mut policy)
        .expect("expire");
    f.assert_same(&stock, &ours);
}

#[test]
fn reflog_expire_never_rewrites_every_entry() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("expire-never");
    f.ok(&stock, &["reflog", "expire", "--expire=never", "refs/heads/main"]);

    backend(&ours, |_| {})
        .reflog_expire(name("refs/heads/main"), ExpireFlags::default(), &mut Prune::default())
        .expect("expire");
    f.assert_same(&stock, &ours);
}

#[test]
fn reflog_delete_one_entry() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("delete-one");
    f.ok(&stock, &["reflog", "delete", "main@{1}"]);

    // `reflog_delete()` (reflog.c:520-570): the 2nd of 3 entries, from the oldest.
    let mut policy = Prune {
        positions: vec![2],
        ..Default::default()
    };
    backend(&ours, |_| {})
        .reflog_expire(name("refs/heads/main"), ExpireFlags::default(), &mut policy)
        .expect("expire");
    f.assert_same(&stock, &ours);
}

#[test]
fn reflog_delete_rewrite_chains_the_kept_entries() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("delete-rewrite");
    f.ok(&stock, &["reflog", "delete", "--rewrite", "main@{1}"]);

    let flags = ExpireFlags {
        rewrite: true,
        ..Default::default()
    };
    let mut policy = Prune {
        positions: vec![2],
        ..Default::default()
    };
    backend(&ours, |_| {})
        .reflog_expire(name("refs/heads/main"), flags, &mut policy)
        .expect("expire");
    f.assert_same(&stock, &ours);
    let old_of_newest = f.ok(&ours, &["log", "-g", "-1", "--format=%H", "main"]);
    assert!(!old_of_newest.is_empty());
}

#[test]
fn reflog_delete_updateref_moves_the_branch() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("delete-updateref");
    f.ok(&stock, &["reflog", "delete", "--updateref", "--rewrite", "main@{0}"]);

    let flags = ExpireFlags {
        update_ref: true,
        rewrite: true,
        ..Default::default()
    };
    let mut policy = Prune {
        positions: vec![3],
        ..Default::default()
    };
    backend(&ours, |_| {})
        .reflog_expire(name("refs/heads/main"), flags, &mut policy)
        .expect("expire");
    f.assert_same(&stock, &ours);
    assert_eq!(rev_parse(&f, &ours, "main"), rev_parse(&f, &ours, "main@{0}"));
    assert_eq!(rev_parse(&f, &ours, "main"), rev_parse(&f, &ours, "side~1"));
}

#[test]
fn reflog_expire_dry_run_changes_nothing() {
    let Some(f) = Fixture::new() else { return };
    let (_, ours) = f.copies("dry-run");
    let before = tables(&ours);
    let flags = ExpireFlags {
        dry_run: true,
        ..Default::default()
    };
    let mut policy = Prune {
        all: true,
        ..Default::default()
    };
    backend(&ours, |_| {})
        .reflog_expire(name("refs/heads/main"), flags, &mut policy)
        .expect("expire");
    assert!(policy.cleaned_up && policy.seen == 3);
    assert_eq!(tables(&ours), before);
    let files = std::fs::read_dir(ours.join(".git/reftable")).expect("dir").count();
    assert_eq!(files, before.len() + 1, "only the tables and tables.list remain");
}

#[test]
fn create_reflog_writes_the_marker_checkout_orphan_does() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("create");
    // `checkout -l --orphan` creates the reflog, then moves HEAD in a table of
    // its own while auto-compaction is off.
    let out = f
        .cmd(
            &stock,
            &[
                "-c",
                "core.logAllRefUpdates=false",
                "checkout",
                "-q",
                "-l",
                "--orphan",
                "newb",
            ],
        )
        .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let be = backend(&ours, |c| c.opts.disable_auto_compact = true);
    be.create_reflog(name("refs/heads/newb")).expect("create");
    let ours_tables = tables(&ours);
    let stock_tables = tables(&stock);
    assert_eq!(ours_tables.len() + 1, stock_tables.len());
    assert_eq!(ours_tables[..], stock_tables[..ours_tables.len()], "the marker table");
    assert!(f.run(&ours, &["reflog", "exists", "refs/heads/newb"]).status.success());
    assert!(f.run(&ours, &["refs", "verify"]).status.success());

    // A reflog that exists is left alone.
    be.create_reflog(name("refs/heads/newb")).expect("create again");
    be.create_reflog(name("refs/heads/main")).expect("create existing");
    assert_eq!(tables(&ours), ours_tables);
}

#[test]
fn delete_reflog_like_reflog_drop() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("drop");
    f.ok(&stock, &["reflog", "drop", "refs/heads/side"]);
    backend(&ours, |_| {})
        .delete_reflog(name("refs/heads/side"))
        .expect("delete");
    f.assert_same(&stock, &ours);
    assert!(!f.run(&ours, &["reflog", "exists", "refs/heads/side"]).status.success());
}

/// git deletes under the name it was given, before `backend_for()` strips the
/// worktree prefix (refs/reftable-backend.c:2486-2491); the stack keeps `HEAD`'s
/// log as `HEAD`, so `main-worktree/HEAD` matches no entry and stays.
#[test]
fn delete_reflog_of_a_prefixed_name_matches_no_entry() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("drop-prefixed");
    f.ok(&stock, &["reflog", "drop", "main-worktree/HEAD"]);
    backend(&ours, |_| {})
        .delete_reflog(name("main-worktree/HEAD"))
        .expect("delete");
    f.assert_same(&stock, &ours);
    assert_eq!(
        f.ok(&ours, &["log", "-g", "--format=%gs", "HEAD"]).lines().count(),
        3,
        "HEAD keeps the entries of its three commits"
    );
}

#[test]
fn rename_like_branch_m() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("rename");
    f.ok(&stock, &["branch", "-m", "side", "side2"]);
    backend(&ours, |_| {})
        .rename_ref(
            name("refs/heads/side"),
            name("refs/heads/side2"),
            committer(),
            "Branch: renamed refs/heads/side to refs/heads/side2".into(),
        )
        .expect("rename");
    f.assert_same(&stock, &ours);
}

#[test]
fn rename_of_the_current_branch_logs_to_head() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("rename-head");
    // `branch -m` renames, then points HEAD at the new name in a table of its own.
    let out = f
        .cmd(&stock, &["branch", "-m", "main", "trunk"])
        .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    backend(&ours, |c| c.opts.disable_auto_compact = true)
        .rename_ref(
            name("refs/heads/main"),
            name("refs/heads/trunk"),
            committer(),
            "Branch: renamed refs/heads/main to refs/heads/trunk".into(),
        )
        .expect("rename");
    let (ours_tables, stock_tables) = (tables(&ours), tables(&stock));
    assert_eq!(ours_tables.len() + 1, stock_tables.len());
    assert_eq!(ours_tables[..], stock_tables[..ours_tables.len()], "the rename table");
}

#[test]
fn copy_like_branch_c() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("copy");
    f.ok(&stock, &["branch", "-c", "side", "side3"]);
    backend(&ours, |_| {})
        .copy_ref(
            name("refs/heads/side"),
            name("refs/heads/side3"),
            committer(),
            "Branch: copied refs/heads/side to refs/heads/side3".into(),
        )
        .expect("copy");
    f.assert_same(&stock, &ours);
}

#[test]
fn rename_conflicts_fail_with_gits_messages() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("conflict");
    f.ok(&stock, &["branch", "a/b"]);
    f.ok(&ours, &["branch", "a/b"]);
    let be = backend(&ours, |_| {});
    let before = tables(&ours);
    for (from, to) in [("side", "main/x"), ("side", "a")] {
        let out = f.run(&stock, &["branch", "-m", from, to]);
        assert!(!out.status.success());
        let stderr = String::from_utf8(out.stderr).expect("utf8");
        let err = be
            .rename_ref(
                name(&format!("refs/heads/{from}")),
                name(&format!("refs/heads/{to}")),
                committer(),
                "m".into(),
            )
            .expect_err("conflict");
        assert_eq!(
            format!("error: {err}"),
            stderr.lines().next().expect("a line"),
            "{from} -> {to}"
        );
    }
    let err = be
        .copy_ref(name("refs/heads/nope"), name("refs/heads/x"), committer(), "m".into())
        .expect_err("missing");
    assert_eq!(err.to_string(), "refname refs/heads/nope not found");
    let err = be
        .copy_ref(name("HEAD"), name("refs/heads/x"), committer(), "m".into())
        .expect_err("symref");
    assert_eq!(
        err.to_string(),
        "refname HEAD is a symbolic ref, copying it is not supported"
    );
    assert_eq!(tables(&ours), before, "nothing was written");
}

#[test]
fn optimize_like_pack_refs() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("pack");
    f.ok(&stock, &["pack-refs"]);
    let be = backend(&ours, |_| {});
    assert!(be.optimize_required(false).expect("required"));
    be.optimize(false).expect("optimize");
    f.assert_same(&stock, &ours);
    assert_eq!(tables(&ours).len(), 1);
    assert!(!be.optimize_required(false).expect("required"));
    let files = std::fs::read_dir(ours.join(".git/reftable")).expect("dir").count();
    assert_eq!(files, 2, "the compacted tables were cleaned up");
}

#[test]
fn optimize_auto_like_pack_refs_auto() {
    let Some(f) = Fixture::new() else { return };
    // Grow the stack without auto-compaction so that it needs some.
    let r = f.root.join("R");
    for b in ["b1", "b2", "b3", "b4"] {
        let out = f
            .cmd(&r, &["branch", b])
            .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
            .output()
            .expect("git runs");
        assert!(out.status.success());
    }
    let (stock, ours) = f.copies("pack-auto");
    f.ok(&stock, &["pack-refs", "--auto"]);
    let be = backend(&ours, |_| {});
    assert!(be.optimize_required(true).expect("required"));
    be.optimize(true).expect("optimize");
    f.assert_same(&stock, &ours);
    assert!(!be.optimize_required(true).expect("required"));
}

/// `<path>: <msg id>: <message>`, how `fsck_report_ref()` prints `r` after its
/// severity.
fn report_line(r: &FsckReport<'_>) -> String {
    format!("{}: {}: {}", r.path, r.msg_id, r.message)
}

/// Whether `msg_id` is an error by default (fsck.h:30-101); the others are
/// warnings or informational.
fn is_error(msg_id: &str) -> bool {
    matches!(msg_id, "badHeadTarget" | "badReferentName" | "badRefOid")
}

/// Lines of `refs verify` without their severity, progress lines as they are.
fn without_severity(stderr: &[u8]) -> Vec<String> {
    String::from_utf8(stderr.to_vec())
        .expect("utf8")
        .lines()
        .filter(|l| *l != "Checking references consistency")
        .map(|l| {
            l.strip_prefix("error: ")
                .or_else(|| l.strip_prefix("warning: "))
                .unwrap_or(l)
                .to_owned()
        })
        .collect()
}

/// Write one table with symbolic refs `name -> target` to the stack of `repo`.
fn add_symrefs(repo: &Path, symrefs: &[(&str, &str)]) {
    let backend = Backend::open(&repo.join(".git"), None, gix_hash::Kind::Sha1);
    let stack = backend.main_stack().expect("stack");
    let mut st = gix_ref::reftable::lock(&stack);
    st.add(
        |wr, st| {
            let ts = st.next_update_index();
            wr.set_limits(ts, ts)?;
            let mut refs: Vec<_> = symrefs
                .iter()
                .map(|(name, target)| gix_reftable::RefRecord {
                    refname: (*name).into(),
                    update_index: ts,
                    value: gix_reftable::RefValue::Symref((*target).into()),
                })
                .collect();
            wr.add_refs(&mut refs)
        },
        None,
    )
    .expect("table");
}

#[test]
fn fsck_reports_like_refs_verify() {
    let Some(f) = Fixture::new() else { return };
    let (stock, ours) = f.copies("fsck");
    for repo in [&stock, &ours] {
        f.ok(repo, &["symbolic-ref", "HEAD", "refs/tags/v1"]);
        add_symrefs(
            repo,
            &[
                ("refs/heads/bad-target", "refs/heads/a..b"),
                ("refs/heads/one-level", "ORIG_HEADX"),
                ("refs/heads/not-a-ref", "foo/bar"),
                ("refs/heads/to-root", "ORIG_HEAD"),
            ],
        );
        let dir = repo.join(".git/reftable");
        let list = std::fs::read_to_string(dir.join("tables.list")).expect("list");
        let first = list.lines().next().expect("a table").to_owned();
        std::fs::rename(dir.join(&first), dir.join("bogus.ref")).expect("rename");
        std::fs::write(dir.join("tables.list"), list.replace(&first, "bogus.ref")).expect("list");
    }
    let out = f.run(&stock, &["refs", "verify", "--verbose"]);
    assert!(!out.status.success(), "refs verify finds errors");
    let stock_lines = without_severity(&out.stderr);

    // Progress and reports share git's stderr in the order they happen.
    let be = backend(&ours, |_| {});
    let lines = std::cell::RefCell::new(Vec::new());
    let found = be
        .fsck(
            None,
            &mut |r| {
                lines.borrow_mut().push(report_line(&r));
                i32::from(is_error(r.msg_id))
            },
            &mut |msg| lines.borrow_mut().push(msg.to_owned()),
        )
        .expect("fsck");
    assert!(found, "errors were reported");
    // Table names end in a random part.
    let unrandom = |lines: Vec<String>| -> Vec<String> {
        lines
            .into_iter()
            .map(|l| match l.strip_prefix("Checking table: 0x") {
                Some(name) => format!("Checking table: 0x{}", name.rsplit_once('-').expect("name").0),
                None => l,
            })
            .collect()
    };
    assert_eq!(unrandom(lines.into_inner()), unrandom(stock_lines));
}

#[test]
fn fsck_reports_a_null_object_id() {
    let Some(f) = Fixture::new() else { return };
    let (_, ours) = f.copies("fsck-null");
    let be = backend(&ours, |_| {});
    {
        let stack = be.main_stack().expect("stack");
        let mut st = gix_ref::reftable::lock(&stack);
        st.add(
            |wr, st| {
                let ts = st.next_update_index();
                wr.set_limits(ts, ts)?;
                wr.add_ref(&gix_reftable::RefRecord {
                    refname: "refs/heads/null".into(),
                    update_index: ts,
                    value: gix_reftable::RefValue::Val1(Default::default()),
                })
            },
            None,
        )
        .expect("table");
    }
    let out = f.run(&ours, &["refs", "verify"]);
    assert!(!out.status.success(), "refs verify finds the error");

    let mut lines = Vec::new();
    let found = be
        .fsck(
            None,
            &mut |r| {
                lines.push(report_line(&r));
                i32::from(is_error(r.msg_id))
            },
            &mut |_| {},
        )
        .expect("fsck");
    assert!(found, "a null object ID is an error");
    assert_eq!(lines, without_severity(&out.stderr));
    assert_eq!(
        lines,
        [format!(
            "refs/heads/null: badRefOid: points to invalid object ID '{}'",
            "0".repeat(40)
        )]
    );
}

#[test]
fn create_and_remove_on_disk() {
    let tmp = gix_testtools::tempfile::Builder::new()
        .prefix("p4-reftable-on-disk-")
        .tempdir()
        .expect("temp dir");
    let git_dir = tmp.path().join("repo.git");
    std::fs::create_dir(&git_dir).expect("mkdir");
    Backend::create_on_disk(&git_dir).expect("create");
    Backend::create_on_disk(&git_dir).expect("existing directories are fine");
    assert_eq!(
        std::fs::read_to_string(git_dir.join("HEAD")).expect("HEAD"),
        "ref: refs/heads/.invalid\n"
    );
    assert_eq!(
        std::fs::read_to_string(git_dir.join("refs/heads")).expect("stub"),
        "this repository uses the reftable format\n"
    );
    assert!(git_dir.join("reftable").is_dir());

    Backend::remove_on_disk(&git_dir).expect("remove");
    let left: Vec<_> = std::fs::read_dir(&git_dir).expect("dir").collect();
    assert!(left.is_empty(), "everything is gone");
    assert_eq!(
        Backend::remove_on_disk(&git_dir).expect_err("stubs are gone"),
        "could not delete stub HEAD: No such file or directory\
         could not delete stub heads: No such file or directory\
         could not delete refs directory: No such file or directory"
    );
}
