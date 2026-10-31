//! `rebase -r` against stock git on a criss-cross history, and the instruction
//! sheet an instruction rewrites under the sequencer.
//!
//! * `do_merge()` hands a merge whose heads have several merge bases to
//!   `merge_ort_recursive()` (sequencer.c:4409-4424), which merges the bases into
//!   a virtual `merged common ancestors` first (merge-ort.c:5313-5385).
//! * `pick_commits()` re-reads `git-rebase-todo` after an `exec`, a `reword` or a
//!   `merge -c` whose editor ran (`reread_todo_if_changed()`, sequencer.c:5006-5027,
//!   5236-5239), keeps the lines it read as written (`save_todo()`,
//!   sequencer.c:3692-3730), and a reschedule writes the sheet from memory.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use std::mem::ManuallyDrop;
use twin_repo::{Out, Side};

const TODO: &str = ".git/rebase-merge/git-rebase-todo";

/// The variants of the criss-cross history built on top of the shared one:
///
/// ```text
/// a:  base - A1 - A2(A1, B1) [- A3] - M(a, b)
/// b:  base - B1 - B2(B1, A1) [- B3]
/// up: base - up
/// ```
///
/// `Clean` touches separate files on each side; `Late` adds `A3`/`B3`, which
/// change the same line of `f`, so recreating `M` conflicts against a clean
/// virtual base; `Nested` has `A1` and `B1` change that line, so `A2` and `B2`
/// conflict and the virtual base of `M` carries conflict markers of its own.
#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Clean,
    Late,
    Nested,
}

const NINE: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";

fn line2(v: &str) -> String {
    NINE.replacen("\n2\n", &format!("\n{v}\n"), 1)
}

/// Both worlds with the same history, built by stock git in each so the
/// fixtures are byte-identical.
fn world(label: &str, shape: Shape) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    let (s, z) = twin_repo::pair(label, stock);
    for side in [&s, &z] {
        // `Side` removes its root on drop; this one only borrows `side`'s.
        let b = ManuallyDrop::new(Side { root: side.root.clone(), bin: stock.to_owned() });
        let git = |args: &[&str]| b.git(args);
        let ok = |args: &[&str]| {
            let out = b.git(args);
            assert_eq!(out.code, 0, "{args:?}: {out:?}");
        };
        let resolve = |body: &str, msg: &str| {
            b.write("f", body);
            ok(&["add", "f"]);
            ok(&["commit", "-q", "-m", msg]);
        };
        ok(&["checkout", "-q", "-b", "base", "main"]);
        b.write("f", NINE);
        ok(&["add", "f"]);
        ok(&["commit", "-q", "-m", "base"]);
        ok(&["branch", "up"]);
        ok(&["checkout", "-q", "-b", "a"]);
        match shape {
            Shape::Nested => b.write("f", &line2("A")),
            _ => b.write("fa", "a1\n"),
        }
        ok(&["add", "."]);
        ok(&["commit", "-q", "-m", "A1"]);
        ok(&["checkout", "-q", "-b", "b", "base"]);
        match shape {
            Shape::Nested => b.write("f", &line2("B")),
            _ => b.write("fb", "b1\n"),
        }
        ok(&["add", "."]);
        ok(&["commit", "-q", "-m", "B1"]);
        ok(&["checkout", "-q", "a"]);
        if git(&["merge", "-q", "--no-edit", "-m", "A2", "b"]).code != 0 {
            resolve(&line2("AB"), "A2");
        }
        ok(&["checkout", "-q", "b"]);
        if git(&["merge", "-q", "--no-edit", "-m", "B2", "a^1"]).code != 0 {
            resolve(&line2("BA"), "B2");
        }
        if shape == Shape::Late {
            b.write("f", &line2("Y"));
            ok(&["commit", "-q", "-am", "B3"]);
            ok(&["checkout", "-q", "a"]);
            b.write("f", &line2("X"));
            ok(&["commit", "-q", "-am", "A3"]);
        }
        ok(&["checkout", "-q", "a"]);
        if git(&["merge", "-q", "--no-edit", "-m", "M", "b"]).code != 0 {
            resolve(&line2("M"), "M");
        }
        ok(&["checkout", "-q", "up"]);
        b.write("u", "u\n");
        ok(&["add", "u"]);
        ok(&["commit", "-q", "-m", "up"]);
        ok(&["checkout", "-q", "a"]);
    }
    Some((s, z))
}

/// What a finished or stopped rebase left behind, read by stock git.
fn post_state(side: &Side, stock: &str) -> Vec<String> {
    let b = ManuallyDrop::new(Side { root: side.root.clone(), bin: stock.to_owned() });
    let mut out = Vec::new();
    for args in [
        &["log", "--graph", "--format=%H %P %s", "--all"][..],
        &["for-each-ref"],
        &["status", "--porcelain"],
        &["status"],
        &["reflog", "-8", "HEAD"],
        &["fsck", "--no-dangling"],
    ] {
        out.push(format!("{:?}", b.git(args)));
    }
    for f in [
        "f",
        ".git/MERGE_HEAD",
        ".git/MERGE_MSG",
        ".git/REBASE_HEAD",
        ".git/rebase-merge/done",
        TODO,
        ".git/rebase-merge/end",
        ".git/rebase-merge/msgnum",
        ".git/rebase-merge/stopped-sha",
    ] {
        out.push(format!("{f}: {:?}", b.read(f).map(|x| String::from_utf8_lossy(&x).into_owned())));
    }
    out
}

/// Run every step on both sides, comparing each answer, then the end state.
fn same(s: &Side, z: &Side, steps: &[(&[(&str, &str)], &[&str])]) {
    let stock = s.bin.clone();
    for (env, args) in steps {
        let (zo, so): (Out, Out) = (z.git_env(env, args), s.git_env(env, args));
        assert_eq!(zo, so, "{args:?}");
    }
    assert_eq!(post_state(z, &stock), post_state(s, &stock));
}

/// The stopped instruction's own `f`, staged, then `--continue` — until the
/// rebase is over.
fn resolve_through(s: &Side, z: &Side) {
    for _ in 0..4 {
        if !s.repo().join(".git/rebase-merge").exists() {
            break;
        }
        let stopped = String::from_utf8(s.read(".git/rebase-merge/stopped-sha").unwrap()).unwrap();
        let spec = format!("{}:f", stopped.trim());
        let body = s.git(&["show", &spec]).stdout;
        for side in [s, z] {
            side.write("f", &body);
        }
        same(s, z, &[(&[], &["add", "f"]), (&[], &["rebase", "--continue"])]);
    }
}

#[test]
fn a_clean_criss_cross_merge_is_recreated_against_the_virtual_base() {
    let Some((s, z)) = world("rebase-crisscross-clean", Shape::Clean) else { return };
    same(&s, &z, &[(&[], &["rebase", "-r", "up"])]);
}

#[test]
fn a_criss_cross_merge_conflict_stops_with_the_merged_ancestors_label() {
    let Some((s, z)) = world("rebase-crisscross-late", Shape::Late) else { return };
    same(
        &s,
        &z,
        &[
            (&[], &["config", "merge.conflictStyle", "diff3"]),
            (&[], &["rebase", "-r", "up"]),
            (&[], &["diff"]),
        ],
    );
    resolve_through(&s, &z);
}

#[test]
fn a_conflicted_virtual_base_nests_its_markers_and_reports_inner_merges() {
    let Some((s, z)) = world("rebase-crisscross-nested", Shape::Nested) else { return };
    same(
        &s,
        &z,
        &[
            (&[], &["config", "merge.conflictStyle", "zdiff3"]),
            (&[], &["config", "merge.verbosity", "5"]),
            (&[], &["rebase", "-r", "up"]),
        ],
    );
    resolve_through(&s, &z);
}

#[test]
fn an_exec_that_appends_to_the_sheet_runs_what_it_appended() {
    let Some((s, z)) = world("rebase-reread-append", Shape::Clean) else { return };
    let exec = format!("echo ran; echo 'exec echo appended' >> {TODO}");
    same(&s, &z, &[(&[], &["rebase", "-r", "up", "-x", &exec])]);
}

#[test]
fn lines_an_exec_writes_are_kept_as_written_in_done_and_status() {
    let Some((s, z)) = world("rebase-reread-raw", Shape::Clean) else { return };
    let exec = format!(
        "perl -i -pe 's/^exec .*/x echo hi/; s/^pick ([0-9a-f]{{7}})[0-9a-f]+/p $1/; \
         $_ = \"break\\n$_\" if $. == 1' {TODO}"
    );
    same(
        &s,
        &z,
        &[
            (&[], &["rebase", "-i", "-r", "up", "-x", &exec]),
            (&[], &["status"]),
            (&[], &["rebase", "--continue"]),
        ],
    );
}

#[test]
fn an_exec_that_removes_truncates_or_breaks_the_sheet() {
    for (label, exec) in [
        ("rm", format!("rm {TODO}")),
        ("truncate", format!(": > {TODO}")),
        ("bogus", format!("echo bogus line >> {TODO}")),
    ] {
        let Some((s, z)) = world(&format!("rebase-reread-{label}"), Shape::Clean) else { return };
        same(
            &s,
            &z,
            &[(&[], &["rebase", "-r", "up", "-x", &exec]), (&[], &["status"]), (&[], &["rebase", "--continue"])],
        );
    }
}

#[test]
fn a_rescheduled_exec_writes_the_sheet_from_memory() {
    let Some((s, z)) = world("rebase-reread-resched", Shape::Clean) else { return };
    let exec = format!("echo 'exec echo late' >> {TODO}; false");
    same(
        &s,
        &z,
        &[
            (&[], &["rebase", "-r", "--reschedule-failed-exec", "up", "-x", &exec]),
            (&[], &["rebase", "--continue"]),
        ],
    );
}

#[test]
fn an_editor_opened_by_merge_c_or_reword_can_rewrite_the_sheet() {
    for (label, sheet_edit) in [
        ("merge-c", "s/^merge -C/merge -c/"),
        ("reword", "s/^pick /reword /"),
    ] {
        let Some((s, z)) = world(&format!("rebase-reread-{label}"), Shape::Clean) else { return };
        let seq = format!("perl -i -pe '{sheet_edit}'");
        let editor = format!("echo 'exec echo from-editor' >> {TODO}; true");
        same(
            &s,
            &z,
            &[(
                &[("GIT_SEQUENCE_EDITOR", seq.as_str()), ("GIT_EDITOR", editor.as_str())],
                &["rebase", "-i", "-r", "up"],
            )],
        );
    }
}

#[test]
fn exec_rebase_edit_todo_feeds_the_edit_back_to_the_running_rebase() {
    let Some((s, z)) = world("rebase-reread-edit-todo", Shape::Clean) else { return };
    let seq = "perl -i -pe 's/^(label onto)$/$1\\nexec git rebase --edit-todo/ if $. == 1; s/^merge -C ([0-9a-f]+) b /merge -c $1 b /'";
    same(
        &s,
        &z,
        &[(
            &[
                ("GIT_SEQUENCE_EDITOR", seq),
                ("GIT_EDITOR", "true"),
            ],
            &["rebase", "-i", "-r", "up"],
        )],
    );
}
