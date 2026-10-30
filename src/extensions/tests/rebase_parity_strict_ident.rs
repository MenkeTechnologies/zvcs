//! The strict identity lookups of the rebase, cherry-pick and revert paths that
//! commit without `do_pick_commit()`'s plain pick, and of `git commit --date`.
//!
//! Every commit git writes goes through `commit_tree_extended()`'s
//! `git_author_info(IDENT_STRICT)` / `git_committer_info(IDENT_STRICT)`
//! (commit.c:1709-1714), or through a `git commit` child that asks the same at
//! builtin/commit.c:777 and :941. An exact replay (`rebase -f` over a linear range),
//! a fixup chain, a `reword`, a `merge -C`, the `--root` stand-in, a pick that went
//! empty and `cherry-pick`/`revert --continue` each die there when the account's
//! default address is bogus — after whatever they had already written, which the
//! comparison checks: the state directory, the pseudo-refs, `MERGE_MSG`,
//! `COMMIT_EDITMSG`, the index and the work tree. `git commit` takes the author
//! date from `--date` or the reused commit before `GIT_AUTHOR_DATE`, so only an
//! unclaimed one is parsed.
//!
//! Only a machine whose default address *is* bogus reaches the deaths; elsewhere
//! both binaries commit with the same synthesized identity, which the comparison
//! still checks. Each case builds one repository with stock git, copies it, and
//! runs the command under both binaries with the identity variables removed and
//! the dates pinned.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

const IDENT_VARS: [&str; 5] = [
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "EMAIL",
];

/// Each of the four variables alone, and the pairs that complete one role.
const ENVS: &[&[(&str, &str)]] = &[
    &[("GIT_COMMITTER_EMAIL", "ce@example.com")],
    &[("GIT_COMMITTER_NAME", "Env C")],
    &[("GIT_AUTHOR_EMAIL", "ae@example.com")],
    &[("GIT_AUTHOR_NAME", "Env A")],
    &[("GIT_AUTHOR_NAME", "Env A"), ("GIT_AUTHOR_EMAIL", "ae@example.com")],
    &[("GIT_COMMITTER_NAME", "Env C"), ("GIT_COMMITTER_EMAIL", "ce@example.com")],
];

struct Fixture {
    root: PathBuf,
    stock: &'static str,
    n: std::cell::Cell<usize>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Exit status, stdout and stderr, plus the post-state stock reads back.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
    refs: String,
    status: String,
    log: String,
    index: String,
    fsck: String,
    /// Every uppercase file under `.git` and every file of a state directory, with
    /// its contents.
    state: Vec<(String, String)>,
}

impl Fixture {
    /// `base`, then `main` adding `a1` and `a2`; `side` adding `s` off `base`;
    /// `topic` adding `t1`, merging `side` and adding `t2`; `dup` re-adding `a1`
    /// off `base`; `side2` and `topic2` changing `f` apart.
    fn new(tag: &str) -> Option<Self> {
        let stock = stock_git::stock_git_at_least((2, 56, 0))?;
        let root = std::env::temp_dir().join(format!("zvcs-rebase-ident-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("seed")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let f = Fixture { root, stock, n: std::cell::Cell::new(0) };
        let seed = f.root.join("seed");
        let git = |args: &[&str]| f.ok(&seed, args);
        let write = |name: &str, body: &str| std::fs::write(seed.join(name), body).unwrap();
        git(&["init", "-q", "-b", "main", "."]);
        write("f", "base\n");
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "base"]);
        git(&["tag", "base"]);
        for name in ["a1", "a2"] {
            write(name, &format!("{name}\n"));
            git(&["add", name]);
            git(&["commit", "-q", "-m", name]);
        }
        git(&["checkout", "-q", "-b", "side", "base"]);
        write("s", "s\n");
        git(&["add", "s"]);
        git(&["commit", "-q", "-m", "side"]);
        git(&["checkout", "-q", "-b", "topic", "base"]);
        write("t1", "t1\n");
        git(&["add", "t1"]);
        git(&["commit", "-q", "-m", "t1"]);
        git(&["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        write("t2", "t2\n");
        git(&["add", "t2"]);
        git(&["commit", "-q", "-m", "t2"]);
        git(&["checkout", "-q", "-b", "dup", "base"]);
        write("a1", "a1\n");
        git(&["add", "a1"]);
        git(&["commit", "-q", "-m", "a1 dup"]);
        git(&["checkout", "-q", "-b", "side2", "base"]);
        write("f", "s2\n");
        git(&["commit", "-q", "-a", "-m", "s2"]);
        git(&["checkout", "-q", "-b", "topic2", "base"]);
        write("f", "c1\n");
        git(&["commit", "-q", "-a", "-m", "c1"]);
        write("f", "c2\n");
        git(&["commit", "-q", "-a", "-m", "c2"]);
        git(&["checkout", "-q", "main"]);
        Some(f)
    }

    /// A command with an identity, as the seed is built.
    fn cmd(&self, bin: &str, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(bin);
        c.args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.root)
            .env("ZVCS_HOME", self.root.join(".zvcs"))
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_EDITOR", "true")
            .env("LC_ALL", "C")
            .env("TZ", "UTC");
        c
    }

    fn ok(&self, dir: &Path, args: &[&str]) {
        let out = self.cmd(self.stock, dir, args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn stock_out(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.cmd(self.stock, dir, args).output().unwrap();
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        s
    }

    fn rev(&self, spec: &str) -> String {
        self.stock_out(&self.root.join("seed"), &["rev-parse", spec]).trim().to_string()
    }

    /// A `GIT_SEQUENCE_EDITOR` that replaces the todo list with `todo`.
    fn todo_editor(&self, name: &str, todo: &str) -> String {
        let path = self.root.join(name);
        std::fs::write(&path, format!("#!/bin/sh\ncat >\"$1\" <<'EOF'\n{todo}EOF\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// Copy the seed, put it in the state `prepare` leaves (run by stock, with an
    /// identity), then run `args` under `bin` with every identity variable removed
    /// and `env` set.
    fn outcome(
        &self,
        bin: &str,
        n: usize,
        prepare: &[&[&str]],
        env: &[(&str, &str)],
        args: &[&str],
    ) -> Outcome {
        let dir = self.root.join(format!("{}-{n}", if bin == ZVCS { "zvcs" } else { "stock" }));
        let cp = Command::new("/bin/cp").arg("-Rp").arg(self.root.join("seed")).arg(&dir).status().unwrap();
        assert!(cp.success());
        for step in prepare {
            // A stop is the point: the preparation's status is not checked.
            let _ = self.cmd(self.stock, &dir, step).output().unwrap();
        }
        let mut c = self.cmd(bin, &dir, args);
        for k in IDENT_VARS {
            c.env_remove(k);
        }
        c.env("GIT_AUTHOR_DATE", "1700000100 +0000").env("GIT_COMMITTER_DATE", "1700000100 +0000");
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        let root = self.root.to_string_lossy().into_owned();
        let clean = |b: &[u8]| String::from_utf8_lossy(b).replace(&root, "<root>");
        let git_dir = dir.join(".git");
        let mut state: Vec<(String, String)> = Vec::new();
        for entry in std::fs::read_dir(&git_dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if name.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
                state.push((name.clone(), clean(&std::fs::read(git_dir.join(&name)).unwrap_or_default())));
            }
        }
        for sub in ["rebase-merge", "sequencer"] {
            let Ok(entries) = std::fs::read_dir(git_dir.join(sub)) else { continue };
            for entry in entries {
                let entry = entry.unwrap();
                let name = format!("{sub}/{}", entry.file_name().to_string_lossy());
                state.push((name, clean(&std::fs::read(entry.path()).unwrap_or_default())));
            }
        }
        state.sort();
        let outcome = Outcome {
            code: out.status.code().expect("no signal"),
            stdout: clean(&out.stdout),
            stderr: clean(&out.stderr),
            refs: self.stock_out(&dir, &["for-each-ref"]),
            status: self.stock_out(&dir, &["status", "--porcelain"]),
            log: self.stock_out(&dir, &["log", "--all", "--format=%H %an <%ae> %ad | %cn <%ce> %cd %s"]),
            index: self.stock_out(&dir, &["ls-files", "-s"]),
            fsck: self.stock_out(&dir, &["fsck", "--no-progress"]),
            state,
        };
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    #[track_caller]
    fn same(&self, prepare: &[&[&str]], env: &[(&str, &str)], args: &[&str]) -> Outcome {
        let n = self.n.get();
        self.n.set(n + 1);
        let stock = self.outcome(self.stock, n, prepare, env, args);
        let zvcs = self.outcome(ZVCS, n, prepare, env, args);
        assert_eq!(zvcs, stock, "git {args:?} after {prepare:?} with {env:?}: left is zvcs, right is stock");
        stock
    }
}

/// `rebase -f` over a linear range re-commits each pick on the parent it already
/// has; git still runs every pick through the sequencer, so a refused committer
/// stops the first one with the state directory written, and `--signoff` dies
/// earlier still, in `append_signoff()`, before the pick is merged.
#[test]
fn exact_replay_stops_at_the_first_pick() {
    let Some(f) = Fixture::new("replay") else { return };
    for env in ENVS {
        f.same(&[], env, &["rebase", "-f", "base"]);
        f.same(&[], env, &["rebase", "--committer-date-is-author-date", "base"]);
        f.same(&[], env, &["rebase", "--signoff", "base"]);
    }
    let got = f.same(&[], ENVS[1], &["rebase", "-f", "base"]);
    if got.code == 128 {
        assert!(got.stderr.contains("Committer identity unknown\n"), "{}", got.stderr);
        assert!(got.state.iter().any(|(name, _)| name == "CHERRY_PICK_HEAD"), "{:?}", got.state);
    }
}

/// A fixup chain commits in process (`try_to_commit()`) and dies with the fixup
/// applied; a squash and a reword hand the commit to a `git commit` child, whose
/// failure is `error_failed_squash()` / `error_with_patch()`'s stop. An editor that
/// fails stops a reword or a squash the same way.
#[test]
fn fixups_squashes_and_rewords_stop_where_git_commits() {
    let Some(f) = Fixture::new("chain") else { return };
    let (a1, a2) = (f.rev("main~1"), f.rev("main"));
    let editors = [
        f.todo_editor("fixup", &format!("pick {a1} a1\nfixup {a2} a2\n")),
        f.todo_editor("fixup-c", &format!("pick {a1} a1\nfixup -C {a2} a2\n")),
        f.todo_editor("squash", &format!("pick {a1} a1\nsquash {a2} a2\n")),
        f.todo_editor("reword", &format!("reword {a1} a1\npick {a2} a2\n")),
        f.todo_editor("edit", &format!("edit {a1} a1\npick {a2} a2\n")),
    ];
    for editor in &editors {
        for env in ENVS {
            let mut env = env.to_vec();
            env.push(("GIT_SEQUENCE_EDITOR", editor));
            f.same(&[], &env, &["rebase", "-i", "base"]);
            f.same(&[], &env, &["rebase", "-i", "-f", "base"]);
        }
        let identity = [
            ("GIT_AUTHOR_NAME", "Env A"),
            ("GIT_AUTHOR_EMAIL", "ae@example.com"),
            ("GIT_COMMITTER_NAME", "Env C"),
            ("GIT_COMMITTER_EMAIL", "ce@example.com"),
            ("GIT_SEQUENCE_EDITOR", editor),
            ("GIT_EDITOR", "false"),
        ];
        f.same(&[], &identity, &["rebase", "-i", "base"]);
    }
}

/// `merge -C` concludes through a `git commit` child; a refused committer is
/// `error_with_patch()`'s stop with `MERGE_HEAD`, `MERGE_MSG` and the merged index
/// left, a zero-byte patch, and the merge recorded in `rewritten-list`.
#[test]
fn merge_command_stops_in_its_commit() {
    let Some(f) = Fixture::new("merge") else { return };
    let (merge, side, t1) = (f.rev("topic~1"), f.rev("side"), f.rev("topic~2"));
    let editors = [
        f.todo_editor("merge", &format!("label onto\nreset onto\nmerge -C {merge} {side} # side\n")),
        f.todo_editor("merge-ff", &format!("label onto\nreset onto\npick {t1}\nmerge -C {merge} {side} # side\n")),
    ];
    for editor in &editors {
        for env in ENVS {
            let mut env = env.to_vec();
            env.push(("GIT_SEQUENCE_EDITOR", editor));
            f.same(&[], &env, &["rebase", "-i", "-r", "base", "topic"]);
            f.same(&[], &env, &["rebase", "-i", "-r", "-f", "base", "topic"]);
        }
    }
}

/// `--root` without `--onto` mints its stand-in with `commit_tree()` before it
/// validates anything else, so an unusable author or committer dies first.
#[test]
fn root_stand_in_asks_for_both() {
    let Some(f) = Fixture::new("root") else { return };
    for env in ENVS {
        f.same(&[], env, &["rebase", "--root"]);
        f.same(&[], env, &["rebase", "--root", "extra", "operand"]);
        f.same(&[], env, &["rebase", "--root", "--onto", "side"]);
    }
}

/// `--continue` and a pick that went empty both end in a `git commit` child: a
/// pick's author is `CHERRY_PICK_HEAD`'s, a revert's is the user's, and the
/// template is in `COMMIT_EDITMSG` before the committer is asked for.
#[test]
fn continue_and_empty_picks_ask_like_git_commit() {
    let Some(f) = Fixture::new("continue") else { return };
    let (c1, a1) = (f.rev("topic2~1"), f.rev("main~1"));
    let pick_stop: &[&[&str]] = &[
        &["checkout", "-q", "side2"],
        &["cherry-pick", &c1],
        &["checkout-index", "-f", "--stage=2", "f"],
        &["add", "f"],
    ];
    let revert_stop: &[&[&str]] = &[
        &["checkout", "-q", "topic2"],
        &["revert", "--no-edit", &c1, &a1],
        &["checkout-index", "-f", "--stage=2", "f"],
        &["add", "f"],
    ];
    let on_dup: &[&[&str]] = &[&["checkout", "-q", "dup"]];
    for env in ENVS {
        f.same(pick_stop, env, &["cherry-pick", "--continue"]);
        f.same(revert_stop, env, &["revert", "--continue"]);
        f.same(on_dup, env, &["cherry-pick", &a1]);
        f.same(on_dup, env, &["cherry-pick", "-x", &a1]);
    }
}

/// `determine_author_info()` parses `--date` first and only then formats the
/// author, whose date is the reused commit's or `--date`'s before
/// `GIT_AUTHOR_DATE`'s, so an unparsable `GIT_AUTHOR_DATE` only matters when
/// nothing claimed the date.
#[test]
fn commit_date_sources_outrank_the_environment() {
    let Some(f) = Fixture::new("date") else { return };
    let identity: &[(&str, &str)] = &[
        ("GIT_AUTHOR_NAME", "Env A"),
        ("GIT_AUTHOR_EMAIL", "ae@example.com"),
        ("GIT_COMMITTER_NAME", "Env C"),
        ("GIT_COMMITTER_EMAIL", "ce@example.com"),
    ];
    let mut envs: Vec<Vec<(&str, &str)>> = ENVS.iter().map(|e| e.to_vec()).collect();
    envs.push(identity.to_vec());
    for mut env in envs {
        env.push(("GIT_AUTHOR_DATE", "garbage"));
        f.same(&[], &env, &["commit", "--allow-empty", "-m", "x"]);
        f.same(&[], &env, &["commit", "--allow-empty", "-m", "x", "--date=2020-01-01T00:00:00"]);
        f.same(&[], &env, &["commit", "--allow-empty", "-m", "x", "--date=notadate"]);
        f.same(&[], &env, &["commit", "--allow-empty", "-m", "x", "--author=X <x@example.com>"]);
        f.same(&[], &env, &["commit", "--amend", "--no-edit"]);
        f.same(&[], &env, &["commit", "--amend", "--no-edit", "--reset-author", "--date=2020-01-01T00:00:00"]);
    }
}
