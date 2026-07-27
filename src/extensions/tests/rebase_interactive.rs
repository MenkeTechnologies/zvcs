//! `git rebase -i` — the instruction sheet and the sequencer that executes it.
//!
//! These are differential tests: the same scripted rebase runs under this binary
//! and under a stock `git`, over byte-identical fixtures with pinned author and
//! committer dates, and the two must agree on stdout, stderr, exit status *and*
//! the resulting object ids. Pinned dates are what makes the id comparison
//! meaningful — a rebase that produced the same trees but different metadata
//! would show up as a different hash.
//!
//! The sequence editor is never a real editor: it is `cp <prepared sheet>`, so
//! each case states the exact instruction stream it wants and the test needs
//! nothing beyond POSIX `cp`/`true`. `GIT_EDITOR=true` accepts whatever message
//! the sequencer prepared, which is what makes `squash`'s combined message
//! reproducible.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A stock git to compare against, or `None` when the machine has no foreign git
/// installed.
///
/// Resolved EXPLICITLY rather than through `PATH`: on a machine where zvcs
/// shadows `git` — the machine this is developed on — `PATH` resolution would
/// silently make the oracle the thing under test.
fn stock_git() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return Path::new(&p).exists().then_some(p);
    }
    ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .map(str::to_owned)
}

/// The environment every invocation runs under: no ambient config, a pinned
/// identity and a pinned clock, so commit ids are a function of content alone.
fn cmd(bin: &str, repo: &Path, home: &Path) -> Command {
    let mut c = Command::new(bin);
    c.current_dir(repo)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env("GIT_AUTHOR_NAME", "Alice")
        .env("GIT_AUTHOR_EMAIL", "alice@example.com")
        .env("GIT_COMMITTER_NAME", "Bob")
        .env("GIT_COMMITTER_EMAIL", "bob@example.com")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0200")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0200");
    c
}

fn run(bin: &str, repo: &Path, home: &Path, args: &[&str]) -> Output {
    cmd(bin, repo, home).args(args).output().unwrap()
}

fn ok(bin: &str, repo: &Path, home: &Path, args: &[&str]) {
    let out = run(bin, repo, home, args);
    assert!(out.status.success(), "{args:?} failed: {}", show(bin, &out));
}

fn show(label: &str, o: &Output) -> String {
    format!(
        "{label}: exit={:?}\n  stdout={:?}\n  stderr={:?}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
    )
}

/// Everything that must match: the two output streams, the exit code, the whole
/// reachable history (ids, trees, parents and messages), the branch tip and the
/// worktree state.
///
/// The history records are sorted rather than compared in walk order. Every
/// commit in these fixtures carries the same pinned committer date, so
/// `log --all`'s tie-break between independent tips is arbitrary and says
/// nothing about the rebase; the *set* of objects and where the branch points
/// are what the rebase decides.
fn snapshot(bin: &str, repo: &Path, home: &Path, o: &Output) -> String {
    let log = run(bin, repo, home, &["log", "--format=%H %T %P%n%B---", "--all"]);
    let text = String::from_utf8_lossy(&log.stdout).into_owned();
    let mut records: Vec<&str> = text.split("---\n").filter(|r| !r.trim().is_empty()).collect();
    records.sort_unstable();
    let head = run(bin, repo, home, &["rev-parse", "HEAD", "topic"]);
    let status = run(bin, repo, home, &["status", "--short"]);
    format!(
        "exit={:?}\nstdout={}\nstderr={}\nhead={}\nlog={}\nstatus={}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
        String::from_utf8_lossy(&head.stdout),
        records.join("---\n"),
        String::from_utf8_lossy(&status.stdout),
    )
}

/// `topic` carries three commits touching `g`; `main` moved `f` underneath it,
/// so `main..topic` is a genuine three-commit replay rather than a fast-forward.
///
/// Built with `builder` so both halves of a differential case are constructed by
/// the *same* binary and any fixture-construction difference cannot masquerade
/// as a rebase difference.
fn fixture(builder: &str, tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-rbi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));

    ok(builder, &repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "a\n").unwrap();
    std::fs::write(repo.join("g"), "x\n").unwrap();
    ok(builder, &repo, &home, &["add", "."]);
    ok(builder, &repo, &home, &["commit", "-q", "-m", "base"]);
    ok(builder, &repo, &home, &["checkout", "-q", "-b", "topic"]);
    for n in ["t1", "t2", "t3"] {
        std::fs::write(repo.join("g"), format!("{n}\n")).unwrap();
        ok(builder, &repo, &home, &["commit", "-q", "-a", "-m", n]);
    }
    ok(builder, &repo, &home, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f"), "a\nm1\n").unwrap();
    ok(builder, &repo, &home, &["commit", "-q", "-a", "-m", "m1"]);
    ok(builder, &repo, &home, &["checkout", "-q", "topic"]);
    (repo, home)
}

/// The `main..topic` commits, oldest first — the order the instruction sheet
/// lists them in, so a case can name them positionally.
fn range(bin: &str, repo: &Path, home: &Path) -> Vec<String> {
    let out = run(bin, repo, home, &["rev-list", "--reverse", "main..topic"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// Run one scripted case under both binaries and require identical results.
///
/// `sheet` receives the `main..topic` ids (oldest first) and returns the exact
/// instruction sheet to hand the sequencer; `body` runs the commands under test.
/// Skipped (with the zvcs half still exercised) when no stock git is installed.
fn differential<S, B>(tag: &str, sheet: S, body: B)
where
    S: Fn(&[String]) -> String,
    B: Fn(&str, &Path, &Path, &Path) -> Output,
{
    let Some(stock) = stock_git() else {
        // Still run the zvcs half, so the case is not silently vacuous.
        let (repo, home) = fixture(BIN, &format!("{tag}-solo"));
        let ids = range(BIN, &repo, &home);
        let path = repo.join(".sheet");
        std::fs::write(&path, sheet(&ids)).unwrap();
        body(BIN, &repo, &home, &path);
        return;
    };
    let mut results = Vec::new();
    for (bin, half) in [(stock.as_str(), "stock"), (BIN, "zvcs")] {
        // Both fixtures are built by the STOCK binary so the starting object ids
        // are identical; only the rebase differs between the two halves.
        let (repo, home) = fixture(&stock, &format!("{tag}-{half}"));
        let ids = range(&stock, &repo, &home);
        let path = repo.join(".sheet");
        std::fs::write(&path, sheet(&ids)).unwrap();
        let out = body(bin, &repo, &home, &path);
        results.push(snapshot(bin, &repo, &home, &out));
    }
    assert_eq!(results[1], results[0], "case `{tag}` diverged from stock git");
}

/// `GIT_SEQUENCE_EDITOR` that installs `sheet` verbatim: git runs the value as
/// `sh -c '<value> "$@"' <value> <todo-path>`, so a bare `cp <sheet>` overwrites
/// the todo file with the prepared instruction stream.
fn editor(sheet: &Path) -> String {
    format!("cp {}", sheet.display())
}

/// A `pick`-only sheet is the sheet git generates, so `-i` over it must land on
/// exactly the commits a plain rebase would — same ids, same order.
#[test]
fn pick_only_matches_stock() {
    differential(
        "pick",
        |ids| ids.iter().map(|id| format!("pick {id}\n")).collect(),
        |bin, repo, home, sheet| {
            cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap()
        },
    );
}

/// `squash` melds the tree *and* the message; `fixup` melds the tree and throws
/// the message away. Running both in one chain pins the combined message that
/// `update_squash_messages()` builds, because a wrong comment marker or a wrong
/// `#N:` header would survive `git commit`'s cleanup and change the commit id.
#[test]
fn squash_then_fixup_matches_stock() {
    differential(
        "squashfixup",
        |ids| format!("pick {}\nsquash {}\nfixup {}\n", ids[0], ids[1], ids[2]),
        |bin, repo, home, sheet| {
            cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap()
        },
    );
}

/// Dropping the middle commit makes the last one's patch no longer apply, which
/// is the cheapest way to reach the conflict stop. The case then resolves and
/// continues, so it pins the whole interrupted round trip: the state directory,
/// the message carried across it, and the id of the commit `--continue` makes.
#[test]
fn drop_conflict_then_continue_matches_stock() {
    differential(
        "dropcontinue",
        |ids| format!("pick {}\ndrop {}\npick {}\n", ids[0], ids[1], ids[2]),
        |bin, repo, home, sheet| {
            let stopped = cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(stopped.status.code(), Some(1), "{}", show(bin, &stopped));
            std::fs::write(repo.join("g"), "resolved\n").unwrap();
            ok(bin, repo, home, &["add", "g"]);
            cmd(bin, repo, home)
                .args(["rebase", "--continue"])
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap()
        },
    );
}

/// `--abort` from the same conflict must put `topic` and the worktree back
/// exactly where they were, leaving no state directory behind.
#[test]
fn abort_from_conflict_restores_original_state() {
    differential(
        "abort",
        |ids| format!("pick {}\ndrop {}\npick {}\n", ids[0], ids[1], ids[2]),
        |bin, repo, home, sheet| {
            let stopped = cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(stopped.status.code(), Some(1), "{}", show(bin, &stopped));
            let out = run(bin, repo, home, &["rebase", "--abort"]);
            assert!(
                !repo.join(".git/rebase-merge").exists(),
                "--abort left a state directory behind"
            );
            out
        },
    );
}

/// `--skip` throws the conflicting commit away rather than resolving it, so the
/// rebase finishes one commit short. The distinction from `--continue` is what
/// this pins: same stop, different tip.
#[test]
fn skip_from_conflict_matches_stock() {
    differential(
        "skip",
        |ids| format!("pick {}\ndrop {}\npick {}\n", ids[0], ids[1], ids[2]),
        |bin, repo, home, sheet| {
            let stopped = cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(stopped.status.code(), Some(1), "{}", show(bin, &stopped));
            run(bin, repo, home, &["rebase", "--skip"])
        },
    );
}

/// `break` hands control back mid-sheet with exit 0 and a resumable state; the
/// following `--continue` must pick up at the next instruction, not re-run the
/// one before it.
#[test]
fn break_stops_and_continue_resumes() {
    differential(
        "break",
        |ids| format!("pick {}\nbreak\npick {}\npick {}\n", ids[0], ids[1], ids[2]),
        |bin, repo, home, sheet| {
            let stopped = cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(stopped.status.code(), Some(0), "{}", show(bin, &stopped));
            assert!(
                repo.join(".git/rebase-merge").exists(),
                "`break` did not leave a resumable rebase"
            );
            cmd(bin, repo, home)
                .args(["rebase", "--continue"])
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap()
        },
    );
}

/// `edit` stops *after* making the commit, with an `amend` marker so the user's
/// follow-up work lands on it. Committing nothing and continuing must therefore
/// leave the commit exactly as the pick made it.
#[test]
fn edit_stops_after_the_pick_and_continue_keeps_it() {
    differential(
        "edit",
        |ids| format!("pick {}\nedit {}\npick {}\n", ids[0], ids[1], ids[2]),
        |bin, repo, home, sheet| {
            let stopped = cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(stopped.status.code(), Some(0), "{}", show(bin, &stopped));
            cmd(bin, repo, home)
                .args(["rebase", "--continue"])
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap()
        },
    );
}

/// `--exec` inserts its command after every pick, and a command that exits
/// non-zero stops the rebase with exit 1 while leaving it resumable.
///
/// The sequence editor is `true`, not a prepared sheet: `--exec`'s lines are
/// added *before* the editor runs, so overwriting the sheet would delete the
/// very instructions under test.
#[test]
fn exec_runs_after_every_pick_and_a_failure_stops() {
    differential(
        "exec",
        |_| String::new(),
        |bin, repo, home, _sheet| {
            let good = cmd(bin, repo, home)
                .args(["rebase", "-i", "--exec", "true", "main"])
                .env("GIT_SEQUENCE_EDITOR", "true")
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(good.status.code(), Some(0), "{}", show(bin, &good));
            ok(bin, repo, home, &["reset", "-q", "--hard", "ORIG_HEAD"]);
            let bad = cmd(bin, repo, home)
                .args(["rebase", "-i", "--exec", "false", "main"])
                .env("GIT_SEQUENCE_EDITOR", "true")
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(bad.status.code(), Some(1), "{}", show(bin, &bad));
            assert!(
                repo.join(".git/rebase-merge").exists(),
                "a failed exec did not leave a resumable rebase"
            );
            bad
        },
    );
}

/// `--autosquash` is the one case where the sheet is *not* prepared by the test:
/// the whole point is that the sequencer rearranges the generated one. The
/// editor is `true`, so what runs is exactly what `todo_list_rearrange_squash()`
/// produced.
#[test]
fn autosquash_rearranges_the_generated_sheet() {
    let Some(stock) = stock_git() else { return };
    let mut results = Vec::new();
    for (bin, half) in [(stock.as_str(), "stock"), (BIN, "zvcs")] {
        let (repo, home) = fixture(&stock, &format!("autosquash-{half}"));
        // A `fixup!` naming the first commit of the range by its subject.
        std::fs::write(repo.join("g"), "t3\nmore\n").unwrap();
        ok(&stock, &repo, &home, &["commit", "-q", "-a", "-m", "fixup! t1"]);
        let out = cmd(bin, &repo, &home)
            .args(["rebase", "-i", "--autosquash", "main"])
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("GIT_EDITOR", "true")
            .output()
            .unwrap();
        results.push(snapshot(bin, &repo, &home, &out));
    }
    assert_eq!(results[1], results[0], "--autosquash diverged from stock git");
}

/// `rebase.missingCommitsCheck=error` turns a deleted instruction line into a
/// refusal: the rebase must not start, and the state directory must stay so
/// `--edit-todo` can fix the sheet.
#[test]
fn missing_commits_check_error_refuses_a_shortened_sheet() {
    differential(
        "misscheck",
        // The middle commit's line is simply absent.
        |ids| format!("pick {}\npick {}\n", ids[0], ids[2]),
        |bin, repo, home, sheet| {
            ok(bin, repo, home, &["config", "rebase.missingCommitsCheck", "error"]);
            let out = cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(1), "{}", show(bin, &out));
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("Dropped commits"),
                "no dropped-commit report: {}",
                show(bin, &out)
            );
            out
        },
    );
}

/// `rebase.missingCommitsCheck=warn` reports the same list but lets the rebase
/// run, which is the whole difference between the two levels.
#[test]
fn missing_commits_check_warn_reports_but_proceeds() {
    differential(
        "misswarn",
        |ids| format!("pick {}\npick {}\n", ids[0], ids[2]),
        |bin, repo, home, sheet| {
            ok(bin, repo, home, &["config", "rebase.missingCommitsCheck", "warn"]);
            let out = cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("Dropped commits"),
                "no dropped-commit report: {}",
                show(bin, &out)
            );
            out
        },
    );
}

/// `rebase.instructionFormat` decides what the sheet's oneline says. `%h` is the
/// interesting placeholder: `sequencer_make_script()` prints through a zeroed
/// `pretty_print_context`, whose `abbrev = 0` means the *full* hash — so a naive
/// abbreviation here would be visibly wrong.
#[test]
fn instruction_format_renders_h_as_a_full_hash() {
    let Some(stock) = stock_git() else { return };
    let mut sheets = Vec::new();
    for (bin, half) in [(stock.as_str(), "stock"), (BIN, "zvcs")] {
        let (repo, home) = fixture(&stock, &format!("instrfmt-{half}"));
        ok(bin, &repo, &home, &["config", "rebase.instructionFormat", "%h %s"]);
        // `cat` shows the sheet and leaves it unchanged, so the rebase still runs.
        let out = cmd(bin, &repo, &home)
            .args(["rebase", "-i", "main"])
            .env("GIT_SEQUENCE_EDITOR", "cat")
            .env("GIT_EDITOR", "true")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let first = text.lines().next().unwrap_or_default().to_string();
        // `pick <abbrev> # <40-hex> t1`: the instruction's own id is abbreviated
        // by `TODO_LIST_SHORTEN_IDS`, while the `%h` the format asked for is not.
        let after_hash = first.split_once('#').map(|(_, r)| r.trim()).unwrap_or_default();
        assert!(
            after_hash.split_whitespace().next().is_some_and(|w| w.len() == 40),
            "`%h` did not render as a full hash: {first:?}"
        );
        sheets.push(text);
    }
    assert_eq!(sheets[1], sheets[0], "rebase.instructionFormat diverged from stock git");
}

/// `rebase.abbreviateCommands` writes the one-letter spellings into the sheet.
/// The sheet must still parse — the round trip through `p`/`s`/`f` is the point.
#[test]
fn abbreviate_commands_writes_short_spellings() {
    let Some(stock) = stock_git() else { return };
    let mut sheets = Vec::new();
    for (bin, half) in [(stock.as_str(), "stock"), (BIN, "zvcs")] {
        let (repo, home) = fixture(&stock, &format!("abbrev-{half}"));
        ok(bin, &repo, &home, &["config", "rebase.abbreviateCommands", "true"]);
        let out = cmd(bin, &repo, &home)
            .args(["rebase", "-i", "main"])
            .env("GIT_SEQUENCE_EDITOR", "cat")
            .env("GIT_EDITOR", "true")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", show(bin, &out));
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            text.starts_with("p "),
            "sheet did not use the abbreviated command: {:?}",
            text.lines().next()
        );
        sheets.push(text);
    }
    assert_eq!(sheets[1], sheets[0], "rebase.abbreviateCommands diverged from stock git");
}

/// `rebase.rescheduleFailedExec` puts a failed `exec` back at the head of the
/// todo list instead of consuming it, so `--continue` retries the same command.
#[test]
fn reschedule_failed_exec_puts_the_command_back() {
    differential(
        "resched",
        |_| String::new(),
        |bin, repo, home, _sheet| {
            ok(bin, repo, home, &["config", "rebase.rescheduleFailedExec", "true"]);
            let out = cmd(bin, repo, home)
                .args(["rebase", "-i", "--exec", "false", "main"])
                .env("GIT_SEQUENCE_EDITOR", "true")
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(1), "{}", show(bin, &out));
            let todo =
                std::fs::read_to_string(repo.join(".git/rebase-merge/git-rebase-todo")).unwrap();
            assert!(
                todo.starts_with("exec false"),
                "the failed exec was not rescheduled: {todo:?}"
            );
            run(bin, repo, home, &["rebase", "--abort"])
        },
    );
}

/// `--edit-todo` re-opens the sheet of a rebase that is already stopped, and the
/// edit has to take effect on the *remaining* instructions only.
#[test]
fn edit_todo_rewrites_the_remaining_instructions() {
    differential(
        "edittodo",
        |ids| format!("pick {}\nedit {}\npick {}\n", ids[0], ids[1], ids[2]),
        |bin, repo, home, sheet| {
            let stopped = cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(stopped.status.code(), Some(0), "{}", show(bin, &stopped));
            // Replace what is left with a single `drop` of the last commit.
            let remaining =
                std::fs::read_to_string(repo.join(".git/rebase-merge/git-rebase-todo")).unwrap();
            let last = remaining.split_whitespace().nth(1).unwrap().to_string();
            let replacement = repo.join(".sheet2");
            std::fs::write(&replacement, format!("drop {last}\n")).unwrap();
            let edited = cmd(bin, repo, home)
                .args(["rebase", "--edit-todo"])
                .env("GIT_SEQUENCE_EDITOR", editor(&replacement))
                .output()
                .unwrap();
            assert_eq!(edited.status.code(), Some(0), "{}", show(bin, &edited));
            cmd(bin, repo, home)
                .args(["rebase", "--continue"])
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap()
        },
    );
}

/// An unknown instruction is a hard refusal, not a silent skip: the sheet is
/// rejected before anything is replayed, and the advice names `--edit-todo`.
#[test]
fn an_invalid_instruction_is_refused() {
    differential(
        "invalid",
        |ids| format!("pick {}\nbogus {}\n", ids[0], ids[1]),
        |bin, repo, home, sheet| {
            let out = cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(1), "{}", show(bin, &out));
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                err.contains("invalid command 'bogus'") && err.contains("--edit-todo"),
                "unexpected refusal: {}",
                show(bin, &out)
            );
            out
        },
    );
}

/// An empty sheet aborts the rebase rather than fast-forwarding: git treats
/// "the user deleted everything" as a cancellation, and leaves the branch alone.
#[test]
fn an_emptied_sheet_aborts_the_rebase() {
    differential(
        "emptysheet",
        |_| String::new(),
        |bin, repo, home, sheet| {
            let out = cmd(bin, repo, home)
                .args(["rebase", "-i", "main"])
                .env("GIT_SEQUENCE_EDITOR", editor(sheet))
                .env("GIT_EDITOR", "true")
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(1), "{}", show(bin, &out));
            assert!(
                !repo.join(".git/rebase-merge").exists(),
                "an emptied sheet left a state directory behind"
            );
            out
        },
    );
}
