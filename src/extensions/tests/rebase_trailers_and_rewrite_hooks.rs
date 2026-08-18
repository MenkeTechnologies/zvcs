//! `git rebase --trailer` and the hooks a rebase owes its caller.
//!
//! Two things a rebase does besides moving commits, both of which were missing:
//!
//! * **`--trailer <t>`** — `amend_strbuf_with_trailers()` runs over every
//!   replayed commit's message (sequencer.c:2436-2437), and over the `fixup -C`
//!   replacement message (sequencer.c:2038-2039). The arguments are validated up
//!   front (`validate_trailer_args()`, builtin/rebase.c:1299-1303) and persisted
//!   in `$state_dir/trailer` so `--continue` keeps applying them.
//! * **the `post-rewrite` hook** — `pick_commits()`'s tail feeds it the
//!   `<old> <new>` pairs it accumulated in `$state_dir/rewritten-list`
//!   (sequencer.c:5190-5207), with `rebase` as `$1`. `try_to_commit()`'s tail
//!   (sequencer.c:1697-1699) additionally runs `post-commit` for every commit the
//!   sequencer writes, and `post-rewrite amend` for every one that amended.
//!
//! Every expectation here was measured against stock git 2.55.0 rather than
//! derived from the source. The hooks are written by the fixture, so nothing
//! depends on the developer's `core.hooksPath` or installed templates, and the
//! scripts are POSIX `sh` with no external commands.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
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
    /// `topic` with two commits, forked from a `main` that has moved on — so a
    /// rebase really replays rather than fast-forwards.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rbtrail-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "c@e.co"]);
        f.git(&["config", "user.name", "C"]);
        f.write("base.txt", "base\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f.git(&["checkout", "-q", "-b", "topic"]);
        f.write("t1.txt", "t1\n");
        f.git(&["add", "t1.txt"]);
        f.git(&["commit", "-q", "-m", "t1 subject"]);
        f.write("t2.txt", "t2\n");
        f.git(&["add", "t2.txt"]);
        f.git(&["commit", "-q", "-m", "t2 subject"]);
        f.git(&["checkout", "-q", "main"]);
        f.write("m1.txt", "m1\n");
        f.git(&["add", "m1.txt"]);
        f.git(&["commit", "-q", "-m", "m1 subject"]);
        f.git(&["checkout", "-q", "topic"]);
        f
    }

    /// `topic` strictly ahead of `main` on one line of development — the shape
    /// that reaches the exact-replay path rather than the sequencer.
    fn linear(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rblin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "c@e.co"]);
        f.git(&["config", "user.name", "C"]);
        f.write("base.txt", "base\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "base"]);
        f.git(&["checkout", "-q", "-b", "topic"]);
        f.write("t1.txt", "t1\n");
        f.git(&["add", "t1.txt"]);
        f.git(&["commit", "-q", "-m", "t1 subject"]);
        f.write("t2.txt", "t2\n");
        f.git(&["add", "t2.txt"]);
        f.git(&["commit", "-q", "-m", "t2 subject"]);
        f
    }

    /// `topic` and `main` both touching `clash.txt`, so replaying `topic`'s one
    /// commit always conflicts. Built once here rather than inline per test.
    fn conflicting(tag: &str) -> Self {
        let f = Fixture::new(tag);
        f.git(&["checkout", "-q", "-B", "topic", "HEAD~2"]);
        f.write("clash.txt", "topic\n");
        f.git(&["add", "clash.txt"]);
        f.git(&["commit", "-q", "-m", "t1 subject"]);
        f.git(&["checkout", "-q", "main"]);
        f.write("clash.txt", "main\n");
        f.git(&["add", "clash.txt"]);
        f.git(&["commit", "-q", "-m", "m2 subject"]);
        f.git(&["checkout", "-q", "topic"]);
        f
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.work.join(name), body).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@e.co")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@e.co")
            .env("GIT_EDITOR", "true");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// `(success, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn exit_code(&self, args: &[&str]) -> (i32, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn rev(&self, spec: &str) -> String {
        let (ok, out, err) = self.run(&["rev-parse", spec]);
        assert!(ok, "rev-parse {spec} failed: {err}");
        out.trim().to_owned()
    }

    /// Raw commit-message bodies of `main..HEAD`, newest first. Read off the
    /// commit objects, not off the rebase's stdout.
    fn messages(&self) -> Vec<String> {
        let (ok, out, err) = self.run(&["log", "--format=%B%x00", "main..HEAD"]);
        assert!(ok, "log failed: {err}");
        out.split('\0')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// The path the hooks append to. Outside the worktree, so a checkout during
    /// the rebase cannot disturb it.
    fn log_path(&self) -> PathBuf {
        self.root.join("hooklog")
    }

    /// Install a hook that appends `<name> <$1>` and then its stdin.
    fn install_hook(&self, name: &str, extra_exit: Option<i32>) {
        let dir = self.work.join(".git/hooks");
        std::fs::create_dir_all(&dir).unwrap();
        let log = self.log_path();
        let body = format!(
            "#!/bin/sh\n\
             {{ echo \"{name} arg=$1\"; cat; }} >> '{}'\n\
             exit {}\n",
            log.display(),
            extra_exit.unwrap_or(0)
        );
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn hooklog(&self) -> String {
        std::fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    /// A `GIT_SEQUENCE_EDITOR` that rewrites the `pick` naming `subject` into
    /// `cmd`. Pure POSIX `sh` — no `perl`, `sed` or `awk`, so it runs on a bare
    /// CI image.
    fn sequence_editor(&self, cmd: &str, subject: &str) -> PathBuf {
        let path = self.root.join(format!("seqed-{}", cmd.replace(' ', "_")));
        let body = format!(
            "#!/bin/sh\n\
             out=\"$1.rewritten\"\n\
             : > \"$out\"\n\
             while IFS= read -r line; do\n\
             \tcase \"$line\" in\n\
             \t\t\"pick \"*\"{subject}\"*) printf '{cmd} %s\\n' \"${{line#pick }}\" >> \"$out\" ;;\n\
             \t\t*) printf '%s\\n' \"$line\" >> \"$out\" ;;\n\
             \tesac\n\
             done < \"$1\"\n\
             mv \"$out\" \"$1\"\n"
        );
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

impl Fixture {
    /// A `GIT_SEQUENCE_EDITOR` that copies the todo list somewhere readable and
    /// leaves it untouched, so a test can assert on exactly what git presented.
    fn todo_capture(&self) -> (PathBuf, PathBuf) {
        let script = self.root.join("seqed-capture");
        let dest = self.root.join("todo.captured");
        let body = format!("#!/bin/sh\ncp \"$1\" '{}'\n", dest.display());
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, dest)
    }

    /// Append a fresh `[core]` section carrying `lines`. Needed for the ordering
    /// case: `git config` rewrites an existing key in place, so it cannot express
    /// "commentString first, then commentChar". A second `[core]` section is
    /// legal and its entries are read after the first one's.
    fn append_core_config(&self, lines: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(self.work.join(".git/config"))
            .unwrap();
        f.write_all(b"\n[core]\n").unwrap();
        f.write_all(lines.as_bytes()).unwrap();
    }

    /// The `Conflicts:` block a stopped pick recorded, as raw lines.
    fn conflict_block(&self, file: &str) -> Vec<String> {
        let path = if file == "MERGE_MSG" {
            self.work.join(".git/MERGE_MSG")
        } else {
            self.work.join(".git/rebase-merge").join(file)
        };
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
            .lines()
            .skip_while(|l| !l.contains("Conflicts:"))
            .map(str::to_owned)
            .collect()
    }
}

/// Parse `hooklog` into `(hook, arg, payload-lines)` records.
fn parse_hooklog(text: &str) -> Vec<(String, String, Vec<String>)> {
    let mut out: Vec<(String, String, Vec<String>)> = Vec::new();
    for line in text.lines() {
        match line.split_once(" arg=") {
            Some((name, arg)) => out.push((name.to_owned(), arg.to_owned(), Vec::new())),
            None => {
                if let Some(last) = out.last_mut() {
                    last.2.push(line.to_owned());
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// --trailer
// ---------------------------------------------------------------------------

/// The reported bug: `--trailer` was accepted, the rebase exited 0, and the
/// trailer was nowhere in the resulting commits. Assert on the commit objects.
#[test]
fn trailer_lands_on_every_replayed_commit() {
    let f = Fixture::new("apply");
    let (ok, out, err) = f.run(&["rebase", "--trailer", "Acked-by: X <x@e.co>", "main"]);
    assert!(ok, "rebase failed: {out}{err}");

    assert_eq!(
        f.messages(),
        vec![
            "t2 subject\n\nAcked-by: X <x@e.co>".to_string(),
            "t1 subject\n\nAcked-by: X <x@e.co>".to_string(),
        ]
    );
}

/// Several `--trailer`s keep their command-line order, and `--signoff` goes on
/// first — `append_signoff()` at sequencer.c:2434 precedes
/// `amend_strbuf_with_trailers()` at :2437.
#[test]
fn trailers_keep_order_and_follow_the_sign_off() {
    let f = Fixture::new("order");
    let (ok, out, err) = f.run(&[
        "rebase",
        "--signoff",
        "--trailer",
        "Acked-by: X <x@e.co>",
        "--trailer",
        "Reviewed-by: Y <y@e.co>",
        "main",
    ]);
    assert!(ok, "rebase failed: {out}{err}");

    assert_eq!(
        f.messages()[0],
        "t2 subject\n\nSigned-off-by: C <c@e.co>\nAcked-by: X <x@e.co>\nReviewed-by: Y <y@e.co>"
    );
}

/// `--trailer` is not a plain append: an argument that repeats the trailer
/// already ending the message is dropped (`ifExists = addIfDifferentNeighbor`).
/// A naive implementation duplicates it, which is exactly what this catches.
#[test]
fn a_trailer_already_present_is_not_duplicated() {
    let f = Fixture::new("dup");
    f.git(&["commit", "-q", "--amend", "-m", "t2 subject\n\nAcked-by: X <x@e.co>"]);

    let (ok, out, err) = f.run(&["rebase", "--trailer", "Acked-by: X <x@e.co>", "main"]);
    assert!(ok, "rebase failed: {out}{err}");

    assert_eq!(f.messages()[0], "t2 subject\n\nAcked-by: X <x@e.co>");
}

/// `validate_trailer_args()` runs before anything is written, so a malformed
/// argument leaves the branch exactly where it was.
#[test]
fn a_malformed_trailer_is_refused_before_the_branch_moves() {
    let f = Fixture::new("validate");
    let before = f.rev("HEAD");

    let (code, err) = f.exit_code(&["rebase", "--trailer", "", "main"]);
    assert_eq!(code, 128, "stderr: {err}");
    assert_eq!(err, "error: empty --trailer argument\n");

    let (code, err) = f.exit_code(&["rebase", "--trailer", "=x", "main"]);
    assert_eq!(code, 128, "stderr: {err}");
    assert_eq!(err, "error: invalid trailer '=x': missing key before separator\n");

    assert_eq!(f.rev("HEAD"), before, "a refused rebase must not move HEAD");
    assert!(!f.work.join(".git/rebase-merge").exists());
}

/// A conflict stop has to carry the trailer across the interruption: git applies
/// it to `ctx->message` *before* the merge, so `$state_dir/message` already has
/// it, and `$state_dir/trailer` lets `--continue` re-derive it in a fresh
/// process. Applying it only on the success path silently dropped it from every
/// hand-resolved commit.
#[test]
fn a_conflicted_pick_keeps_the_trailer_across_continue() {
    let f = Fixture::conflicting("conflict");

    let (ok, _, _) = f.run(&["rebase", "--signoff", "--trailer", "Acked-by: X <x@e.co>", "main"]);
    assert!(!ok, "the pick was supposed to conflict");

    let state = f.work.join(".git/rebase-merge");
    assert_eq!(
        std::fs::read_to_string(state.join("trailer")).unwrap(),
        "Acked-by: X <x@e.co>\n"
    );
    let message = std::fs::read_to_string(state.join("message")).unwrap();
    assert!(
        message.starts_with("t1 subject\n\nSigned-off-by: C <c@e.co>\nAcked-by: X <x@e.co>\n"),
        "the stopped message must already carry both trailers: {message:?}"
    );

    f.write("clash.txt", "resolved\n");
    f.git(&["add", "clash.txt"]);
    let (ok, out, err) = f.run(&["rebase", "--continue"]);
    assert!(ok, "--continue failed: {out}{err}");
    assert_eq!(
        f.messages()[0],
        "t1 subject\n\nSigned-off-by: C <c@e.co>\nAcked-by: X <x@e.co>"
    );
}

/// `fixup -C` replaces the message outright, so `do_pick_commit()`'s
/// `!is_fixup(command)` guards skip it; `append_squash_message()` is the one
/// place it can pick the trailers up (sequencer.c:2029-2039). Without that site
/// a `fixup -C` chain lost both the sign-off and the trailer.
#[test]
fn fixup_dash_c_takes_the_trailer_from_append_squash_message() {
    let f = Fixture::new("fixupc");
    let editor = f.sequence_editor("fixup -C", "t2 subject");

    let out = f
        .cmd(&["rebase", "-i", "--signoff", "--trailer", "Acked-by: X <x@e.co>", "main"])
        .env("GIT_SEQUENCE_EDITOR", &editor)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rebase failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // The chain melded into one commit, and it kept `t2`'s message plus both
    // trailers.
    let msgs = f.messages();
    assert_eq!(msgs.len(), 1, "the fixup should have melded: {msgs:?}");
    assert_eq!(
        msgs[0],
        "t2 subject\n\nSigned-off-by: C <c@e.co>\nAcked-by: X <x@e.co>"
    );
}

/// The linear-ahead shape takes the exact-replay path rather than the sequencer;
/// it used to refuse `--trailer` outright with exit 1.
#[test]
fn trailer_works_on_the_exact_replay_shape() {
    let f = Fixture::linear("exact");
    let (ok, out, err) = f.run(&["rebase", "--trailer", "Acked-by: X <x@e.co>", "main"]);
    assert!(ok, "rebase failed: {out}{err}");

    assert_eq!(
        f.messages(),
        vec![
            "t2 subject\n\nAcked-by: X <x@e.co>".to_string(),
            "t1 subject\n\nAcked-by: X <x@e.co>".to_string(),
        ]
    );
}

// ---------------------------------------------------------------------------
// post-rewrite / post-commit
// ---------------------------------------------------------------------------

/// The reported bug: the hook never ran. It must fire once, with `rebase` as
/// `$1` and one `<old> <new>` line per replayed commit, oldest first.
#[test]
fn post_rewrite_reports_every_replayed_commit() {
    let f = Fixture::new("hook");
    f.install_hook("post-rewrite", None);
    let old_tip = f.rev("HEAD");
    let old_first = f.rev("HEAD~1");

    let (ok, out, err) = f.run(&["rebase", "main"]);
    assert!(ok, "rebase failed: {out}{err}");
    let new_tip = f.rev("HEAD");
    let new_first = f.rev("HEAD~1");

    let records = parse_hooklog(&f.hooklog());
    assert_eq!(records.len(), 1, "exactly one post-rewrite run: {records:?}");
    assert_eq!(records[0].0, "post-rewrite");
    assert_eq!(records[0].1, "rebase");
    assert_eq!(
        records[0].2,
        vec![
            format!("{old_first} {new_first}"),
            format!("{old_tip} {new_tip}"),
        ]
    );
}

/// A fixup chain rewrites two commits into one, so both old ids map to the same
/// new commit — that pairing is the whole reason `rewritten-pending` exists.
/// The melded commit also reports itself through `post-rewrite amend`.
#[test]
fn a_fixup_chain_maps_both_old_commits_onto_the_melded_one() {
    let f = Fixture::new("fixupmap");
    f.install_hook("post-rewrite", None);
    let editor = f.sequence_editor("fixup", "t2 subject");
    let old_tip = f.rev("HEAD");
    let old_first = f.rev("HEAD~1");

    let out = f
        .cmd(&["rebase", "-i", "main"])
        .env("GIT_SEQUENCE_EDITOR", &editor)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rebase failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let melded = f.rev("HEAD");

    let records = parse_hooklog(&f.hooklog());
    let rebase: Vec<_> = records.iter().filter(|r| r.1 == "rebase").collect();
    assert_eq!(rebase.len(), 1, "records: {records:?}");
    assert_eq!(
        rebase[0].2,
        vec![
            format!("{old_first} {melded}"),
            format!("{old_tip} {melded}"),
        ],
        "both rewritten commits must map onto the melded one"
    );

    let amend: Vec<_> = records.iter().filter(|r| r.1 == "amend").collect();
    assert_eq!(amend.len(), 1, "the fixup amends once: {records:?}");
    assert_eq!(amend[0].2.len(), 1);
    assert!(
        amend[0].2[0].ends_with(&melded),
        "the amend pair must name the melded commit: {:?}",
        amend[0].2
    );
}

/// `post-commit` fires for every commit the sequencer writes — this port builds
/// those commit objects itself instead of re-entering `git commit`, so the hook
/// has to be run explicitly (`try_to_commit()`, sequencer.c:1697).
#[test]
fn post_commit_fires_once_per_replayed_commit() {
    let f = Fixture::new("postcommit");
    f.install_hook("post-commit", None);

    let (ok, out, err) = f.run(&["rebase", "main"]);
    assert!(ok, "rebase failed: {out}{err}");

    let records = parse_hooklog(&f.hooklog());
    assert_eq!(records.len(), 2, "one per replayed commit: {records:?}");
    assert!(records.iter().all(|r| r.0 == "post-commit"));
}

/// The hook's exit status is dropped: `run_hooks_opt()`'s return value is not
/// assigned at sequencer.c:5206, so a failing `post-rewrite` cannot fail a
/// rebase that has already rewritten history.
#[test]
fn a_failing_post_rewrite_hook_does_not_fail_the_rebase() {
    let f = Fixture::new("hookfail");
    f.install_hook("post-rewrite", Some(3));

    let (ok, out, err) = f.run(&["rebase", "main"]);
    assert!(ok, "a failing post-rewrite must not fail the rebase: {out}{err}");
    assert!(!f.hooklog().is_empty(), "the hook still ran");
    assert!(!f.work.join(".git/rebase-merge").exists(), "state left behind");
}

/// Nothing was rewritten, so `rewritten-list` is empty and the hook is skipped
/// altogether — `if (!stat(...) && st.st_size > 0)` at sequencer.c:5191.
#[test]
fn a_rebase_that_rewrites_nothing_runs_no_hook() {
    let f = Fixture::new("noop");
    f.install_hook("post-rewrite", None);

    let (ok, out, err) = f.run(&["rebase", "HEAD"]);
    assert!(ok, "rebase failed: {out}{err}");
    assert_eq!(f.hooklog(), "", "no commit was rewritten, so no hook may run");
}

/// An `edit` stop returns before `record_in_rewritten()` (sequencer.c:4957-4967);
/// `--continue` records the commit from `$state_dir/stopped-sha` instead
/// (:5505-5514). That file holds the commit being *replayed*, which is also what
/// `REBASE_HEAD` names — recording the replacement instead produced a useless
/// `<new> <new>` pair.
#[test]
fn an_edit_stop_records_the_original_commit_on_continue() {
    let f = Fixture::new("editstop");
    f.install_hook("post-rewrite", None);
    let editor = f.sequence_editor("edit", "t2 subject");
    let old_tip = f.rev("HEAD");
    let old_first = f.rev("HEAD~1");

    let out = f
        .cmd(&["rebase", "-i", "main"])
        .env("GIT_SEQUENCE_EDITOR", &editor)
        .output()
        .unwrap();
    assert!(out.status.success(), "the edit stop is a success, not a failure");
    assert_eq!(f.hooklog(), "", "nothing is reported while the rebase is stopped");

    let state = f.work.join(".git/rebase-merge");
    assert_eq!(
        std::fs::read_to_string(state.join("stopped-sha")).unwrap().trim(),
        old_tip,
        "stopped-sha names the commit being replayed"
    );
    assert_eq!(f.rev("REBASE_HEAD"), old_tip);

    let (ok, out, err) = f.run(&["rebase", "--continue"]);
    assert!(ok, "--continue failed: {out}{err}");
    let new_tip = f.rev("HEAD");
    let new_first = f.rev("HEAD~1");

    let records = parse_hooklog(&f.hooklog());
    assert_eq!(records.len(), 1, "records: {records:?}");
    assert_eq!(
        records[0].2,
        vec![
            format!("{old_first} {new_first}"),
            format!("{old_tip} {new_tip}"),
        ]
    );
}

/// The exact-replay path never builds a state directory, so it has to carry the
/// pairs itself. It is the shape `--signoff` on an already-linear branch takes,
/// and it ran no hook at all.
#[test]
fn the_exact_replay_path_reports_its_rewrites_too() {
    let f = Fixture::linear("exacthook");
    f.install_hook("post-rewrite", None);
    f.install_hook("post-commit", None);
    let old_tip = f.rev("HEAD");
    let old_first = f.rev("HEAD~1");

    let (ok, out, err) = f.run(&["rebase", "--signoff", "main"]);
    assert!(ok, "rebase failed: {out}{err}");
    let new_tip = f.rev("HEAD");
    let new_first = f.rev("HEAD~1");
    assert_ne!(new_tip, old_tip, "--signoff must rewrite the commits");

    let records = parse_hooklog(&f.hooklog());
    let rebase: Vec<_> = records.iter().filter(|r| r.0 == "post-rewrite").collect();
    assert_eq!(rebase.len(), 1, "records: {records:?}");
    assert_eq!(rebase[0].1, "rebase");
    assert_eq!(
        rebase[0].2,
        vec![
            format!("{old_first} {new_first}"),
            format!("{old_tip} {new_tip}"),
        ]
    );
    assert_eq!(
        records.iter().filter(|r| r.0 == "post-commit").count(),
        2,
        "one post-commit per replayed commit: {records:?}"
    );
}

/// `core.hooksPath` moves the whole hook directory; the rebase must follow it
/// rather than looking only under `.git/hooks`.
#[test]
fn post_rewrite_honours_core_hooks_path() {
    let f = Fixture::new("hookspath");
    f.install_hook("post-rewrite", None);
    // Move the installed hook out of `.git/hooks` and point config at the new home.
    let alt = f.work.join("myhooks");
    std::fs::create_dir_all(&alt).unwrap();
    std::fs::rename(f.work.join(".git/hooks/post-rewrite"), alt.join("post-rewrite")).unwrap();
    f.git(&["config", "core.hooksPath", "myhooks"]);

    let (ok, out, err) = f.run(&["rebase", "main"]);
    assert!(ok, "rebase failed: {out}{err}");
    let records = parse_hooklog(&f.hooklog());
    assert_eq!(records.len(), 1, "records: {records:?}");
    assert_eq!(records[0].1, "rebase");
}

// ---------------------------------------------------------------------------
// core.commentChar / core.commentString
// ---------------------------------------------------------------------------
//
// `append_conflicts_hint()` writes the block with `comment_line_str`
// (sequencer.c:721-744), and the commit-message cleanup later strips lines
// starting with that same string. Writing a literal `#` under any other setting
// therefore did not merely look wrong — the cleanup no longer matched, so the
// whole `Conflicts:` block survived `--continue` into the commit object. Every
// expectation below was measured against stock git 2.55.0.

/// The regression that motivated all of this: under a non-default comment
/// character the block must carry it, and must still disappear from the commit.
#[test]
fn a_conflict_block_uses_the_configured_comment_char() {
    let f = Fixture::conflicting("cc-pipe");
    f.git(&["config", "core.commentChar", "|"]);

    let (ok, _, _) = f.run(&["rebase", "main"]);
    assert!(!ok, "the pick was supposed to conflict");

    // `strbuf_commented_addf` adds a space after the prefix unless the line
    // starts with `\n` or `\t`, so the header is `<c> Conflicts:` and each path
    // is `<c>\t<path>` (add_lines(), strbuf.c:374-391).
    let expected = vec!["| Conflicts:".to_string(), "|\tclash.txt".to_string()];
    assert_eq!(f.conflict_block("message"), expected);
    assert_eq!(f.conflict_block("MERGE_MSG"), expected);

    f.write("clash.txt", "resolved\n");
    f.git(&["add", "clash.txt"]);
    let (ok, out, err) = f.run(&["rebase", "--continue"]);
    assert!(ok, "--continue failed: {out}{err}");

    // The payload: the block is gone from the committed object. Before the fix
    // the message was written with `#` while cleanup stripped `|`, so every
    // conflicted rebase on a machine with this setting committed the block.
    assert_eq!(f.messages()[0], "t1 subject");
}

/// `core.commentChar` and `core.commentString` are one knob in git 2.55
/// (`git_default_core_config()`, environment.c:435-456) and the value is stored
/// whole — it is not a character. Truncating `//` to `/` produced a prefix the
/// reader would not recognise.
#[test]
fn a_multi_character_comment_string_is_not_truncated() {
    for (key, value, prefix) in [
        ("core.commentChar", "//", "//"),
        ("core.commentString", ";", ";"),
        ("core.commentChar", ";;;", ";;;"),
    ] {
        let f = Fixture::conflicting("cc-multi");
        f.git(&["config", key, value]);

        let (ok, _, _) = f.run(&["rebase", "main"]);
        assert!(!ok, "the pick was supposed to conflict");
        assert_eq!(
            f.conflict_block("message"),
            vec![format!("{prefix} Conflicts:"), format!("{prefix}\tclash.txt")],
            "{key}={value}"
        );

        f.write("clash.txt", "resolved\n");
        f.git(&["add", "clash.txt"]);
        let (ok, out, err) = f.run(&["rebase", "--continue"]);
        assert!(ok, "--continue failed for {key}={value}: {out}{err}");
        assert_eq!(f.messages()[0], "t1 subject", "{key}={value}");
    }
}

/// Both spellings feed the same variable, so the *last* one set wins whichever
/// it is — not `commentString` unconditionally.
#[test]
fn the_last_comment_key_set_wins_whichever_spelling_it_is() {
    for (lines, prefix) in [
        ("\tcommentString = \"@\"\n\tcommentChar = \"|\"\n", "|"),
        ("\tcommentChar = \"|\"\n\tcommentString = \"@\"\n", "@"),
    ] {
        let f = Fixture::conflicting("cc-order");
        f.append_core_config(lines);

        let (ok, _, _) = f.run(&["rebase", "main"]);
        assert!(!ok, "the pick was supposed to conflict");
        assert_eq!(
            f.conflict_block("message"),
            vec![format!("{prefix} Conflicts:"), format!("{prefix}\tclash.txt")],
            "config was:\n{lines}"
        );
    }
}

/// `auto` resolves to `#` everywhere outside `git commit`: it sets
/// `auto_comment_line_char` and leaves `comment_line_str = "#"`
/// (environment.c:441-443), and only `builtin/commit.c`'s
/// `adjust_comment_line_char()` — called from `prepare_to_commit()`, after the
/// template is written — ever revises it. The sequencer never calls it.
#[test]
fn auto_leaves_the_sequencers_own_comment_char_at_hash() {
    let f = Fixture::conflicting("cc-auto");
    f.git(&["config", "core.commentChar", "auto"]);

    let (ok, _, _) = f.run(&["rebase", "main"]);
    assert!(!ok, "the pick was supposed to conflict");
    assert_eq!(
        f.conflict_block("message"),
        vec!["# Conflicts:".to_string(), "#\tclash.txt".to_string()]
    );

    f.write("clash.txt", "resolved\n");
    f.git(&["add", "clash.txt"]);
    let (ok, out, err) = f.run(&["rebase", "--continue"]);
    assert!(ok, "--continue failed: {out}{err}");
    assert_eq!(f.messages()[0], "t1 subject");
}

/// The todo list is written *and re-read* with the configured prefix. A fix that
/// wrote `|` but still recognised only `#` on the way back in would leave
/// `--edit-todo` parsing the entire help block as instructions; a fix that got
/// the prefix wrong in either direction changes what survives the round trip.
#[test]
fn the_todo_list_round_trips_under_a_custom_comment_char() {
    let f = Fixture::new("cc-todo");
    f.git(&["config", "core.commentChar", "|"]);
    let (editor, captured) = f.todo_capture();

    let out = f
        .cmd(&["rebase", "-i", "main"])
        .env("GIT_SEQUENCE_EDITOR", &editor)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rebase failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let todo = std::fs::read_to_string(&captured).unwrap();
    let instructions: Vec<&str> = todo
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('|'))
        .collect();
    assert_eq!(instructions.len(), 2, "only the two picks are instructions: {todo}");
    assert!(instructions.iter().all(|l| l.starts_with("pick ")), "{todo}");
    // The help block is commented with `|`, and nothing in it is left on `#`.
    assert!(
        todo.contains("| Commands:"),
        "the help block must carry the configured prefix: {todo}"
    );
    assert!(
        !todo.lines().any(|l| l.starts_with('#')),
        "no line may still be commented with '#': {todo}"
    );

    // The rebase ran the sheet rather than treating the help as instructions.
    assert_eq!(
        f.messages(),
        vec!["t2 subject".to_string(), "t1 subject".to_string()]
    );
}
