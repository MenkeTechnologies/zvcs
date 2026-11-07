//! The commit-limiting, ordering, grep, parent-count, side and date options
//! `cherry-pick` and `revert` hand to `setup_revisions()`
//! (`PARSE_OPT_KEEP_UNKNOWN_OPT`, builtin/revert.c:174), and the pathspecs a
//! `--stdin` read takes after a `--` line, compared with stock git (see
//! `support/sequencer_diff.rs`).
//!
//! `prepare_revs()` walks what `setup_revisions()` set up (sequencer.c:2597-2610),
//! so `--author`, `--grep`, `--merges`, `--first-parent`, `--ancestry-path`,
//! `--left-only`, `--cherry-pick`, `--since`, `--max-count-oldest` and the rest
//! shape the commits replayed, while one plain `<commit>` still takes
//! `single_pick()` past all of them (sequencer.c:5577-5600). A `--` on the
//! command line is eaten by `parse_options()`, so the word after it is a bad
//! revision; only `--stdin` can carry a pathspec (revision.c:3018-3019), which
//! `parse_pathspec()` judges before `run_sequencer()`'s usage check
//! (revision.c:3135-3150). The stdin read runs with
//! `warn_on_object_refname_ambiguity` cleared (revision.c:2984-2985), so a
//! 40-hex ref name is warned about once, by `sequencer_pick_revisions()`'s second
//! resolution, rather than twice.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/sequencer_diff.rs"]
mod sequencer_diff;

use sequencer_diff::{assert_same, run};
use stock_git::stock_git;

/// `main`: A, B (by Bob), C. `other` from A: O1, O2 (by Bob, `sub/p.txt`), O3,
/// a merge of `m2` (Q1 by Bob, from O2), O5 (adds `b.txt` exactly as B does, so
/// `--cherry-pick` pairs the two) and O6 (by Bob, `sub/r.txt`).
const HISTORY: &str = "cb() { mkdir -p \"$(dirname \"$1\")\"; printf '%b' \"$2\" >\"$1\"; git add \"$1\"; tick; GIT_AUTHOR_NAME=Bob git commit -q -m \"$3\"; }
c a.txt 'a\\n' A
cb b.txt 'b\\n' B
c a.txt 'a2\\n' C
git checkout -q -b other main~2
c o1.txt 'o\\n' O1
cb sub/p.txt 'p\\n' O2
c o3.txt 'o3\\n' O3
git checkout -q -b m2 other~1
cb q.txt 'q\\n' Q1
git checkout -q other
tick; git merge -q --no-ff -m Merge m2
c b.txt 'b\\n' O5
cb sub/r.txt 'r\\n' O6
git checkout -q main
";

fn check(tag: &str, script: &str, steps: &[&str]) {
    let Some(stock) = stock_git() else {
        eprintln!("skipping: no stock git");
        return;
    };
    assert_same(&run(stock, tag, script, steps));
}

/// Each option under both verbs, with `--no-commit` so the selection is what is
/// compared; [`committing_walks`] drives the committing modes.
fn both(tag: &str, args: &str) {
    for cmd in ["cherry-pick", "revert"] {
        check(&format!("{tag}-{cmd}"), HISTORY, &[&format!("git {cmd} -n {args}")]);
    }
}

#[test]
fn committing_walks() {
    for (tag, args) in [
        ("author", "--no-merges --author=Bob main..other"),
        ("cherry-pick", "--no-merges --cherry-pick main...other"),
        ("stdin-path", "--stdin <<EOF\nmain..other\n--\nsub\nEOF"),
    ] {
        for (mode, cmd) in [
            ("pick", "cherry-pick"),
            ("pick-x", "cherry-pick -x"),
            ("pick-e", "cherry-pick -e"),
            ("revert", "revert"),
            ("revert-e", "revert -e"),
        ] {
            check(&format!("commit-{tag}-{mode}"), HISTORY, &[&format!("git {cmd} {args}")]);
        }
    }
    // A walk that stops on a conflict, then is continued and skipped.
    let conflict = format!("{HISTORY}mkdir -p sub; c sub/r.txt 'mine\\n' M\n");
    for cmd in ["cherry-pick", "revert"] {
        check(
            &format!("conflict-{cmd}"),
            &conflict,
            &[&format!("git {cmd} --no-merges --author=Bob main..other"), &format!("git {cmd} --skip")],
        );
    }
}

#[test]
fn grep_and_ident_filters() {
    for (tag, args) in [
        ("author", "--no-merges --author=Bob main..other"),
        ("author-single", "--author=Bob other"),
        ("author-list", "--author=Bob other~1 other"),
        ("committer", "--committer=nobody main..other"),
        ("grep", "--no-merges --grep=O main..other"),
        ("grep-i", "--no-merges -i --grep=o main..other"),
        ("grep-invert", "--grep=O --invert-grep main..other"),
        ("grep-all-match", "--grep=O --author=Bob --all-match main..other"),
        ("grep-ere", "-E --grep='O[0-9]' main..other"),
        ("grep-fixed", "-F --no-merges --grep=O main..other"),
        ("grep-bad", "--grep='[' main..other"),
        ("author-no-value", "main..other --author"),
        ("author-eats-range", "--author main..other"),
    ] {
        both(tag, args);
    }
}

#[test]
fn parents_order_and_dates() {
    for (tag, args) in [
        ("merges", "--merges -m1 main..other"),
        ("no-merges", "--no-merges main..other"),
        ("min-parents", "--min-parents=2 -m1 main..other"),
        ("max-parents", "--max-parents=1 main..other"),
        ("min-parents-bad", "--min-parents=x main..other"),
        ("first-parent", "--first-parent --no-merges main..other"),
        ("topo", "--topo-order --no-merges main..other"),
        ("date-order", "--date-order --no-merges main..other"),
        ("author-date-order", "--author-date-order --no-merges main..other"),
        ("oldest", "--max-count-oldest=2 main..other"),
        ("oldest-detached", "--max-count-oldest 2 main..other"),
        ("oldest-skip", "--skip=1 --max-count-oldest=2 main..other"),
        ("since", "--no-merges --since=@1700000400 main..other"),
        ("until", "--until=@1700000400 main..other"),
        ("min-age", "--min-age=1700000400 main..other"),
        ("since-reverse", "--reverse --no-merges --since=@1700000400 main..other"),
    ] {
        both(tag, args);
    }
}

#[test]
fn ancestry_and_sides() {
    for (tag, args) in [
        ("ancestry", "--ancestry-path main..other"),
        ("ancestry-bottom", "--no-merges --ancestry-path other~3..other"),
        ("ancestry-arg-bad", "--ancestry-path=nosuch main..other"),
        ("ancestry-empty", "--ancestry-path --stdin </dev/null"),
        ("left-only", "--left-only main...other"),
        ("right-only", "--no-merges --right-only main...other"),
        ("cherry-pick", "--no-merges --cherry-pick main...other"),
        ("cherry-mark", "--no-merges --cherry-mark main...other"),
        ("cherry", "--cherry main...other"),
        ("not-symmetric", "--no-merges --left-only other --not main...other~3"),
        ("mark-conflict", "--cherry-pick --cherry-mark main...other"),
        ("side-conflict", "--left-only --right-only main...other"),
    ] {
        both(tag, args);
    }
}

#[test]
fn stdin_pathspecs() {
    for (tag, input) in [
        ("range", "main..other\\n--\\nsub"),
        ("single", "other\\n--\\nsub"),
        ("list", "other~2\\nother\\n--\\nsub"),
        ("two-paths", "main..other\\n--\\nsub/r.txt\\no3.txt"),
        ("no-paths", "main..other\\n--"),
        ("no-match", "main..other\\n--\\nnomatch"),
        ("bad-magic", "main..other\\n--\\n:(bogus)x"),
    ] {
        both(&format!("stdin-{tag}"), &format!("--stdin <<EOF\n$(printf '{input}')\nEOF"));
    }
    // The pathspec is judged before the usage an unknown option earns.
    check("stdin-bad-magic-unknown", HISTORY, &["git revert -x --stdin <<EOF\nmain..other\n--\n:(bogus)x\nEOF"]);
    // A `--` on the command line is `parse_options()`'s, so `sub` is a revision.
    both("argv-dashdash", "main..other -- sub");
}

#[test]
fn stdin_skips_the_first_full_hex_warning() {
    let script = format!("{HISTORY}git branch \"$(git rev-parse other~3)\" other\n");
    for cmd in ["cherry-pick", "revert"] {
        for (tag, input) in [
            ("hex", "$(git rev-parse other~3)"),
            ("range", "main..$(git rev-parse other~3)"),
            ("symmetric", "$(git rev-parse other~3)...main"),
        ] {
            check(
                &format!("amb-{tag}-{cmd}"),
                &script,
                &[&format!("git {cmd} -n --stdin <<EOF\n{input}\nEOF")],
            );
        }
        check(&format!("amb-argv-{cmd}"), &script, &[&format!("git {cmd} -n $(git rev-parse other~3)")]);
    }
}
