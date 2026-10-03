//! Round trips between this crate and a stock git that writes reftables.
//!
//! Every test builds its repository with stock git (`init --ref-format=reftable`,
//! available since git 2.45) and checks that tables git wrote read back as git
//! reports them, and that tables this crate wrote are read by git as intended.
//! The strongest check is `rewrite_is_byte_identical`: re-writing the records
//! of a table git compacted, with git's options, reproduces its bytes exactly.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use gix_reftable::{
    HashId, LogRecord, LogUpdate, LogValue, RefRecord, RefValue, Stack, StackOptions, WriteOptions, Writer,
    record::Hash,
    table::Table,
};

/// A stock git that can create reftable repositories. `git` on `PATH` may be
/// the binary under development, so well-known install locations come first.
fn stock_git() -> Option<PathBuf> {
    let candidates = std::env::var_os("ZVCS_STOCK_GIT")
        .map(PathBuf::from)
        .into_iter()
        .chain(["/opt/homebrew/bin/git", "/usr/local/bin/git", "/usr/bin/git"].map(PathBuf::from));
    for git in candidates {
        let probe = tempfile::tempdir().ok()?;
        let ok = Command::new(&git)
            .args(["init", "-q", "--ref-format=reftable"])
            .arg(probe.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", probe.path())
            .output()
            .is_ok_and(|o| o.status.success());
        if ok {
            return Some(git);
        }
    }
    None
}

struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    git: PathBuf,
}

impl Repo {
    fn new() -> Option<Repo> {
        let Some(git) = stock_git() else {
            eprintln!("skipped: no stock git with reftable support");
            return None;
        };
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("repo");
        let repo = Repo { _tmp: tmp, dir, git };
        repo.git_in(repo._tmp.path(), &["init", "-q", "-b", "main", "--ref-format=reftable", "repo"]);
        Some(repo)
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new(&self.git)
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self._tmp.path())
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.dir, args)
    }

    fn commit(&self, msg: &str) {
        self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
    }

    fn stack(&self, opts: &StackOptions) -> Stack {
        Stack::new(&self.dir.join(".git/reftable"), opts).unwrap()
    }

    fn table_files(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(".git/reftable/tables.list"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn hex(h: &[u8]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Hash {
    let mut h = [0u8; 32];
    for (i, b) in h.iter_mut().take(s.len() / 2).enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    h
}

/// All live refs of `stack` as `name value` lines, symrefs as `name -> target`,
/// skipping deletion records as git's backend does (reftable-backend.c:654-655).
fn dump_refs(stack: &Stack) -> Vec<String> {
    let mut it = stack.ref_iterator().unwrap();
    it.seek_ref(b"").unwrap();
    let mut r = RefRecord::default();
    let mut out = Vec::new();
    while it.next_ref(&mut r).unwrap() {
        out.push(match &r.value {
            RefValue::Val1(h) | RefValue::Val2 { value: h, .. } => format!("{} {}", r.refname, hex(&h[..20])),
            RefValue::Symref(t) => format!("{} -> {t}", r.refname),
            RefValue::Deletion => continue,
        });
    }
    out
}

/// The log entries of `refname`, newest first, as `old new message`, skipping
/// deletion records as git's backend does (reftable-backend.c:2283-2286).
fn dump_log(stack: &Stack, refname: &str) -> Vec<String> {
    let mut it = stack.log_iterator().unwrap();
    it.seek_log(refname.as_bytes()).unwrap();
    let mut l = LogRecord::default();
    let mut out = Vec::new();
    while it.next_log(&mut l).unwrap() && l.refname == refname {
        let Some(u) = l.update() else { continue };
        out.push(format!("{} {} {}", hex(&u.old_hash[..20]), hex(&u.new_hash[..20]), String::from_utf8_lossy(u.message_or_empty())));
    }
    out
}

/// A repository with branches, an annotated tag, a detached-free HEAD and
/// reflogs, spread over several tables.
fn populated() -> Option<Repo> {
    let repo = Repo::new()?;
    repo.commit("one");
    repo.commit("two");
    repo.git(&["branch", "topic"]);
    repo.git(&["tag", "-a", "-m", "annotated", "v1"]);
    repo.git(&["tag", "light"]);
    repo.commit("three");
    repo.git(&["update-ref", "-m", "by hand", "refs/heads/other", "HEAD~2"]);
    Some(repo)
}

#[test]
fn reads_what_stock_git_wrote() {
    let Some(repo) = populated() else { return };
    assert!(repo.table_files().len() > 1, "several additions leave several tables before compaction");
    let stack = repo.stack(&StackOptions::default());

    let mut expected: Vec<String> = repo
        .git(&["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .map(str::to_owned)
        .collect();
    expected.push("HEAD -> refs/heads/main".into());
    expected.sort();
    let mut got = dump_refs(&stack);
    got.sort();
    assert_eq!(got, expected);

    // Annotated tags are stored with their peeled value.
    let tag = stack.read_ref(b"refs/tags/v1").unwrap().unwrap();
    let peeled = repo.git(&["rev-parse", "v1^{}"]);
    assert_eq!(hex(&tag.val2().expect("VAL2 record")[..20]), peeled.trim());

    // Reflogs, newest first, agree with what git reports about them.
    let expected: Vec<String> = repo
        .git(&["log", "-g", "--format=%H %gs", "refs/heads/main"])
        .lines()
        .map(str::to_owned)
        .collect();
    let got: Vec<String> = dump_log(&stack, "refs/heads/main")
        .into_iter()
        .map(|l| {
            let mut parts = l.splitn(3, ' ');
            let (_old, new, msg) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
            format!("{new} {}", msg.trim_end())
        })
        .collect();
    assert_eq!(got, expected);
    assert_eq!(
        dump_log(&stack, "refs/heads/other").len(),
        1,
        "update-ref -m wrote one entry"
    );
    assert!(stack.read_ref(b"refs/heads/missing").unwrap().is_none());
}

#[test]
fn reads_compacted_and_indexed_tables() {
    let Some(repo) = populated() else { return };
    let head = repo.git(&["rev-parse", "HEAD"]);
    // Enough refs for many ref blocks, which brings a ref index and an object index.
    let input: String = (0..3000)
        .map(|i| format!("create refs/heads/bulk/{i:05} {}\n", head.trim()))
        .collect();
    let mut child = Command::new(&repo.git)
        .args(["update-ref", "--stdin"])
        .current_dir(&repo.dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", repo._tmp.path())
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), input.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    repo.git(&["pack-refs", "--all"]);
    assert_eq!(repo.table_files().len(), 1, "pack-refs compacts the whole stack");

    let stack = repo.stack(&StackOptions::default());
    let expected = repo.git(&["for-each-ref", "--format=%(refname) %(objectname)"]).lines().count();
    assert_eq!(dump_refs(&stack).len(), expected + 1);

    // Point lookups go through the index levels.
    for name in ["refs/heads/bulk/00000", "refs/heads/bulk/01777", "refs/heads/bulk/02999"] {
        let r = stack.read_ref(name.as_bytes()).unwrap().expect("present");
        assert_eq!(hex(&r.val1().unwrap()[..20]), head.trim());
    }
    assert!(stack.read_ref(b"refs/heads/bulk/03000").unwrap().is_none());
    assert!(stack.read_ref(b"refs/heads/bulk/0").unwrap().is_none());

    // The object index finds every ref pointing at a commit.
    let table = &stack.tables()[0];
    let mut it = table.refs_for(&unhex(head.trim())[..20]).unwrap();
    let mut r = RefRecord::default();
    let mut n = 0;
    while it.next_ref(&mut r).unwrap() {
        n += 1;
    }
    assert!(n >= 3000, "refs_for found {n}");

    // Multi-level ref index, object index, and compressed log blocks, too.
    assert_rewrite_identical(&repo);
}

/// Re-write all records of the single table of `repo` with git's default
/// options and check the bytes against the file git wrote.
fn assert_rewrite_identical(repo: &Repo) {
    let [name] = repo.table_files().try_into().unwrap();
    let path = repo.dir.join(".git/reftable").join(&name);
    let original = std::fs::read(&path).unwrap();

    let table = Table::new(gix_reftable::blocksource::BlockSource::from_buf(original.clone()), &name).unwrap();
    let mut refs = Vec::new();
    let mut it = table.ref_iterator();
    it.seek_ref(b"").unwrap();
    let mut r = RefRecord::default();
    while it.next_ref(&mut r).unwrap() {
        refs.push(r.clone());
    }
    let mut logs = Vec::new();
    let mut it = table.log_iterator();
    it.seek_log(b"").unwrap();
    let mut l = LogRecord::default();
    while it.next_log(&mut l).unwrap() {
        logs.push(l.clone());
    }
    assert!(!logs.is_empty());

    let mut w = Writer::new(Vec::new(), table.hash_id(), &WriteOptions::default()).unwrap();
    w.set_limits(table.min_update_index(), table.max_update_index()).unwrap();
    w.add_refs(&mut refs).unwrap();
    w.add_logs(&mut logs).unwrap();
    w.close().unwrap();
    assert!(w.into_sink() == original, "re-written table differs from git's");
}

#[test]
fn rewrite_is_byte_identical() {
    let Some(repo) = populated() else { return };
    repo.git(&["pack-refs", "--all"]);
    assert_rewrite_identical(&repo);
}

fn log_update(new: &str, message: &str, time: u64) -> LogValue {
    LogValue::Update(LogUpdate {
        new_hash: unhex(new),
        old_hash: [0; 32],
        name: "C O Mitter".into(),
        email: "committer@example.com".into(),
        time,
        tz_offset: -700,
        message: Some(message.into()),
    })
}

#[test]
fn stock_git_reads_what_was_written() {
    let Some(repo) = populated() else { return };
    let head = repo.git(&["rev-parse", "HEAD"]).trim().to_owned();
    let tag = repo.git(&["rev-parse", "v1"]).trim().to_owned();
    let peeled = repo.git(&["rev-parse", "v1^{}"]).trim().to_owned();
    let opts = WriteOptions {
        // Keep every addition as its own table, like a busy stack.
        disable_auto_compact: true,
        ..Default::default()
    };
    let mut stack = repo.stack(&StackOptions::default());

    stack
        .add(
            |w, st| {
                let ts = st.next_update_index();
                w.set_limits(ts, ts)?;
                w.add_ref(&RefRecord {
                    refname: "refs/heads/added".into(),
                    update_index: ts,
                    value: RefValue::Val1(unhex(&head)),
                })?;
                w.add_ref(&RefRecord {
                    refname: "refs/tags/added-tag".into(),
                    update_index: ts,
                    value: RefValue::Val2 {
                        value: unhex(&tag),
                        target_value: unhex(&peeled),
                    },
                })?;
                w.add_log(&LogRecord {
                    refname: "refs/heads/added".into(),
                    update_index: ts,
                    value: log_update(&head, "branch: Created by hand", 1112912000),
                })
            },
            Some(&opts),
        )
        .unwrap();
    // Delete a branch git created, with its reflog, in a second table.
    stack
        .add(
            |w, st| {
                let ts = st.next_update_index();
                w.set_limits(ts, ts)?;
                w.add_ref(&RefRecord {
                    refname: "refs/heads/topic".into(),
                    update_index: ts,
                    value: RefValue::Deletion,
                })?;
                let mut it = st.log_iterator()?;
                it.seek_log(b"refs/heads/topic")?;
                let mut l = LogRecord::default();
                let mut tombstones = Vec::new();
                while it.next_log(&mut l)? && l.refname == "refs/heads/topic" {
                    tombstones.push(LogRecord {
                        refname: l.refname.clone(),
                        update_index: l.update_index,
                        value: LogValue::Deletion,
                    });
                }
                w.add_logs(&mut tombstones)
            },
            Some(&opts),
        )
        .unwrap();

    let refs = repo.git(&["for-each-ref", "--format=%(refname) %(objectname) %(*objectname)"]);
    assert!(refs.contains(&format!("refs/heads/added {head} \n")), "{refs}");
    assert!(refs.contains(&format!("refs/tags/added-tag {tag} {peeled}\n")), "{refs}");
    assert!(!refs.contains("refs/heads/topic"), "{refs}");
    assert_eq!(
        repo.git(&["reflog", "show", "--format=%H %gs", "refs/heads/added"]),
        format!("{head} branch: Created by hand\n")
    );
    assert!(!repo.dir.join(".git/logs/refs/heads/topic").exists());
    let out = Command::new(&repo.git)
        .args(["reflog", "exists", "refs/heads/topic"])
        .current_dir(&repo.dir)
        .output()
        .unwrap();
    assert!(!out.status.success(), "the reflog of the deleted branch is gone");
    repo.git(&["fsck", "--strict", "--no-progress"]);
    repo.git(&["refs", "verify"]);

    // Compacting everything keeps what git sees, and git keeps working on top.
    let before = repo.git(&["for-each-ref"]);
    stack.compact_all(Some(&opts), None).unwrap();
    assert_eq!(repo.table_files().len(), 1);
    assert_eq!(repo.git(&["for-each-ref"]), before);
    repo.commit("four");
    stack.reload().unwrap();
    assert_eq!(
        hex(&stack.read_ref(b"refs/heads/main").unwrap().unwrap().val1().unwrap()[..20]),
        repo.git(&["rev-parse", "main"]).trim()
    );
}

fn add_symref(stack: &mut Stack, name: &str, target: &str, opts: Option<&WriteOptions>) -> gix_reftable::Result<()> {
    stack.add(
        |w, st| {
            let ts = st.next_update_index();
            w.set_limits(ts, ts)?;
            w.add_ref(&RefRecord {
                refname: name.into(),
                update_index: ts,
                value: RefValue::Symref(target.into()),
            })
        },
        opts,
    )
}

/// Since 2.56 an addition reloads a stack that is out of date once locked
/// (`reftable_stack_init_addition()`, stack.c:702-710) instead of failing.
#[test]
fn stale_addition_reloads() {
    let Some(repo) = populated() else { return };
    let mut stale = repo.stack(&StackOptions::default());
    repo.commit("behind the stack's back");
    let main = repo.git(&["rev-parse", "main"]);
    add_symref(&mut stale, "refs/heads/y", "refs/heads/main", None).unwrap();
    assert_eq!(repo.git(&["symbolic-ref", "refs/heads/y"]), "refs/heads/main\n");
    assert_eq!(
        repo.git(&["rev-parse", "main"]),
        main,
        "the concurrent commit survives the stale writer"
    );
    assert_eq!(
        hex(&stale.read_ref(b"refs/heads/main").unwrap().unwrap().val1().unwrap()[..20]),
        main.trim(),
        "the stale stack caught up"
    );
    repo.git(&["refs", "verify"]);
}

/// The `tables.list` lock lives on the stack (stack.h:20-24); an addition that
/// failed to take it must not release the lock another addition holds
/// (`locked`, stack.c:643-648, 671-673).
#[test]
fn failed_addition_keeps_the_other_additions_lock() {
    let Some(repo) = populated() else { return };
    let mut stack = repo.stack(&StackOptions::default());
    let lock = repo.dir.join(".git/reftable/tables.list.lock");
    let opts = WriteOptions::default();

    let mut first = stack.addition_new(Some(&opts)).unwrap();
    assert!(lock.is_file(), "the addition holds the list lock");
    assert_eq!(
        stack.addition_new(Some(&opts)).err(),
        Some(gix_reftable::Error::Lock),
        "a second addition fails at once with a zero timeout"
    );
    assert_eq!(
        add_symref(&mut stack, "refs/heads/z", "refs/heads/main", Some(&opts)),
        Err(gix_reftable::Error::Lock)
    );
    assert!(lock.is_file(), "the failed additions left the first one's lock in place");
    stack.reload().unwrap();

    first
        .add(&stack, |w, st| {
            let ts = st.next_update_index();
            w.set_limits(ts, ts)?;
            w.add_ref(&RefRecord {
                refname: "refs/heads/w".into(),
                update_index: ts,
                value: RefValue::Symref("refs/heads/main".into()),
            })
        })
        .unwrap();
    first.commit(&mut stack).unwrap();
    assert!(!lock.exists(), "committing released the lock");
    assert_eq!(repo.git(&["symbolic-ref", "refs/heads/w"]), "refs/heads/main\n");

    // A dropped addition releases its lock, too.
    let dropped = stack.addition_new(Some(&opts)).unwrap();
    drop(dropped);
    assert!(!lock.exists());
    add_symref(&mut stack, "refs/heads/z", "refs/heads/main", Some(&opts)).unwrap();
    repo.git(&["refs", "verify"]);
}

/// `suppress_deletions` became a stack option in 2.56 (reftable-stack.h:46,
/// stack.c:341), off by default: git's backend skips deletions itself.
#[test]
fn deletions_are_suppressed_only_on_request() {
    let Some(repo) = populated() else { return };
    let out = Command::new(&repo.git)
        .args(["branch", "-D", "topic"])
        .current_dir(&repo.dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", repo._tmp.path())
        .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let find_topic = |opts: &StackOptions| {
        let stack = repo.stack(opts);
        let mut it = stack.ref_iterator().unwrap();
        it.seek_ref(b"refs/heads/topic").unwrap();
        let mut r = RefRecord::default();
        let found = it.next_ref(&mut r).unwrap() && r.refname == "refs/heads/topic";
        assert!(stack.read_ref(b"refs/heads/topic").unwrap().is_none());
        found.then_some(r)
    };
    let tombstone = find_topic(&StackOptions::default()).expect("the deletion record is visible");
    assert!(tombstone.is_deletion());
    assert!(
        find_topic(&StackOptions {
            suppress_deletions: true,
            ..Default::default()
        })
        .is_none(),
        "the merged iterator hides it"
    );
}

/// `on_reload` runs after every reload (stack.c:498-499), including the
/// initial one, and not when the stack is up to date.
#[test]
fn on_reload_runs_per_reload() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let Some(repo) = populated() else { return };
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let mut stack = repo.stack(&StackOptions {
        on_reload: Some(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })),
        ..Default::default()
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1, "opening the stack loads it");
    stack.reload().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "an up-to-date stack is not reloaded");
    repo.commit("four");
    stack.reload().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}


#[test]
fn sha256_tables() {
    let Some(git) = stock_git() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repo {
        dir: tmp.path().join("repo"),
        _tmp: tmp,
        git,
    };
    repo.git_in(
        repo._tmp.path(),
        &["init", "-q", "--object-format=sha256", "--ref-format=reftable", "repo"],
    );
    repo.commit("one");
    let stack = repo.stack(&StackOptions {
        hash_id: HashId::Sha256,
        ..Default::default()
    });
    let main = stack.read_ref(b"refs/heads/master").unwrap().or(stack.read_ref(b"refs/heads/main").unwrap());
    let head = repo.git(&["rev-parse", "HEAD"]);
    assert_eq!(hex(main.unwrap().val1().unwrap()), head.trim());
    assert!(
        Stack::new(&repo.dir.join(".git/reftable"), &StackOptions::default()).is_err(),
        "a SHA-1 stack refuses SHA-256 tables"
    );
}

/// The placeholder `reflog expire --expire=all` leaves in an emptied reflog
/// has a `NULL` message (`write_reflog_expiry_table()`,
/// refs/reftable-backend.c:2548-2560), which `reftable_writer_add_log()`
/// (reftable/writer.c:441-497) stores as the empty string without the newline
/// every other message gets. Re-writing git's expiry table with that record's
/// message as `None` reproduces its bytes; as `Some("")` it is `"\n"`.
#[test]
fn a_missing_log_message_is_stored_empty() {
    let Some(repo) = Repo::new() else { return };
    repo.commit("one");
    repo.commit("two");
    let out = Command::new(&repo.git)
        .args(["reflog", "expire", "--expire=all", "refs/heads/main"])
        .current_dir(&repo.dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", repo._tmp.path())
        .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let name = repo.table_files().pop().unwrap();
    let original = std::fs::read(repo.dir.join(".git/reftable").join(&name)).unwrap();
    let table = Table::new(gix_reftable::blocksource::BlockSource::from_buf(original.clone()), &name).unwrap();
    let mut logs = Vec::new();
    let mut it = table.log_iterator();
    it.seek_log(b"").unwrap();
    let mut l = LogRecord::default();
    while it.next_log(&mut l).unwrap() {
        logs.push(l.clone());
    }
    let is_marker = |u: &LogUpdate| u.old_hash == [0; 32] && u.new_hash == [0; 32];
    let marker = logs
        .iter()
        .filter_map(LogRecord::update)
        .find(|u| is_marker(u))
        .expect("the emptied reflog keeps a placeholder");
    assert_eq!(marker.message.as_ref().map(|m| m.len()), Some(0), "read back, the message is empty");

    let rewrite = |message: Option<&str>| {
        let mut logs = logs.clone();
        for log in &mut logs {
            if let LogValue::Update(u) = &mut log.value {
                if is_marker(u) {
                    u.message = message.map(Into::into);
                }
            }
        }
        let mut w = Writer::new(Vec::new(), table.hash_id(), &WriteOptions::default()).unwrap();
        w.set_limits(table.min_update_index(), table.max_update_index()).unwrap();
        w.add_logs(&mut logs).unwrap();
        w.close().unwrap();
        w.into_sink()
    };
    assert!(rewrite(None) == original, "NULL is stored as git stores it");
    assert!(rewrite(Some("")) != original, "an empty message gets its newline");
}
