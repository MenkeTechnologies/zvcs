//! Two things `git blame` gets exactly right and that are easy to get subtly wrong.
//!
//! The `previous <commit> <path>` field of the porcelain formats is
//! `blame_origin::previous`, which `pass_blame()` (blame.c:2474-2487) sets from
//! `first_scapegoat()` — the commit's parents walking backwards, its children under
//! `--reverse`. `write_filename_info()` (builtin/blame.c:233-242) prints the field if
//! and only if that pointer is set, so a commit the walk never passed blame *through*
//! prints none: the `boundary` of a range-limited blame, and the newest commit under
//! `--reverse`. Deriving the field from `commit^` instead reports one where git
//! reports nothing.
//!
//! The other is `usage(str_usage)` at builtin/blame.c:1210, the answer to a `-L` spec
//! `parse_range_arg()` could not parse. `usage()` is not `usage_with_options()`:
//! `usage_builtin()` (usage.c:45-48) writes `usage: ` plus the one string it was
//! handed, so the whole of stderr is the synopsis line — 62 bytes for `blame`,
//! not the 2097-byte option block a `parse_options` rejection prints.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Identity and date env vars git honours above the config file; a CI runner that
/// exports any of them would otherwise change the commits this fixture builds.
const PINNED_ENV: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "author"),
    ("GIT_AUTHOR_EMAIL", "a@e.co"),
    ("GIT_COMMITTER_NAME", "author"),
    ("GIT_COMMITTER_EMAIL", "a@e.co"),
    ("GIT_AUTHOR_DATE", "2005-04-07T22:13:13+0000"),
    ("GIT_COMMITTER_DATE", "2005-04-07T22:13:13+0000"),
];

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    for (key, value) in PINNED_ENV {
        cmd.env(key, value);
    }
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn git(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = run(dir, home, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn rev_parse(dir: &Path, home: &Path, rev: &str) -> String {
    git(dir, home, &["rev-parse", rev]).trim().to_string()
}

/// Four commits over one file, the second of which renames it and edits it in the
/// same step — the only situation in which `previous` names a path that is not the
/// one being blamed.
fn fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();

    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "a@e.co"]);
    git(&repo, &home, &["config", "user.name", "author"]);

    std::fs::write(repo.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    git(&repo, &home, &["add", "f.txt"]);
    git(&repo, &home, &["commit", "-qm", "c1"]);

    git(&repo, &home, &["mv", "f.txt", "g.txt"]);
    std::fs::write(repo.join("g.txt"), "one\nTWO\nthree\n").unwrap();
    git(&repo, &home, &["add", "g.txt"]);
    git(&repo, &home, &["commit", "-qm", "c2"]);

    std::fs::write(repo.join("g.txt"), "one\nTWO\nTHREE\n").unwrap();
    git(&repo, &home, &["add", "g.txt"]);
    git(&repo, &home, &["commit", "-qm", "c3"]);

    std::fs::write(repo.join("g.txt"), "ONE\nTWO\nTHREE\n").unwrap();
    git(&repo, &home, &["add", "g.txt"]);
    git(&repo, &home, &["commit", "-qm", "c4"]);

    (repo, home)
}

/// The `previous`/`filename` block that follows a given commit's header line, as a
/// list of `"<key> <value>"` strings — one entry per group the commit heads.
fn path_blocks(porcelain: &str, commit: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut lines = porcelain.lines().peekable();
    while let Some(line) = lines.next() {
        let header: Vec<&str> = line.split(' ').collect();
        // A group header is `<sha> <orig> <final> <num-lines>`; the per-line headers
        // inside a group drop the count, and a content line starts with a tab.
        if header.len() != 4 || header[0] != commit {
            continue;
        }
        if header[1..].iter().any(|n| n.parse::<u32>().is_err()) {
            continue;
        }
        let mut block = Vec::new();
        while let Some(next) = lines.peek() {
            if next.starts_with('\t') {
                break;
            }
            let next = lines.next().unwrap();
            if next.starts_with("previous ") || next.starts_with("filename ") {
                block.push(next.to_string());
            }
            if next.starts_with("filename ") {
                break;
            }
        }
        out.push(block);
    }
    out
}

#[test]
fn porcelain_previous_names_the_pre_rename_path_and_stops_at_a_boundary() {
    let (repo, home) = fixture("blame-previous-boundary");
    let c1 = rev_parse(&repo, &home, "HEAD~3");
    let c2 = rev_parse(&repo, &home, "HEAD~2");
    let c3 = rev_parse(&repo, &home, "HEAD~1");
    let c4 = rev_parse(&repo, &home, "HEAD");

    // Unlimited: every blamed commit here handed its entries to a parent, so each one
    // reports a `previous`, and c2's names the name the file had before the rename.
    let full = git(&repo, &home, &["blame", "-p", "g.txt"]);
    assert_eq!(
        path_blocks(&full, &c2),
        vec![vec![format!("previous {c1} f.txt"), "filename g.txt".to_string()]],
        "c2 renamed f.txt to g.txt, so its `previous` must name f.txt:\n{full}"
    );
    assert_eq!(
        path_blocks(&full, &c4),
        vec![vec![format!("previous {c3} g.txt"), "filename g.txt".to_string()]],
        "c4's blame was passed to c3:\n{full}"
    );

    // Limited to c3..c4: c3 is now the boundary. `pass_blame()` is never reached for
    // it, so `suspect->previous` stays NULL and the block is `boundary` then
    // `filename` with nothing in between — even though c3's parent c2 does still hold
    // `g.txt`, which is what makes this the case an object-database lookup gets wrong.
    let bounded = git(&repo, &home, &["blame", "-p", "HEAD~1..HEAD", "g.txt"]);
    assert_eq!(
        path_blocks(&bounded, &c3),
        vec![vec!["filename g.txt".to_string()]],
        "the boundary commit reports no `previous` even though c2 holds g.txt:\n{bounded}"
    );
    assert!(
        bounded.contains("boundary\nfilename g.txt\n") && !bounded.contains(&format!("previous {c2}")),
        "nothing below the range bottom may be named:\n{bounded}"
    );

    // Limited to c2..c4, where the boundary's parent does *not* hold the path.
    let limited = git(&repo, &home, &["blame", "-p", "HEAD~2..HEAD", "g.txt"]);
    assert!(
        limited.contains("boundary\nfilename g.txt\n"),
        "the boundary commit must report no `previous`:\n{limited}"
    );
    assert_eq!(
        path_blocks(&limited, &c2),
        vec![vec!["filename g.txt".to_string()]],
        "c2 is the boundary of this range:\n{limited}"
    );
    assert_eq!(
        path_blocks(&limited, &c4),
        vec![vec![format!("previous {c3} g.txt"), "filename g.txt".to_string()]],
        "c4 is still interior and keeps its `previous`:\n{limited}"
    );
    assert!(
        !limited.contains(&format!("previous {c1}")),
        "c1 is below the range bottom and must not be named at all:\n{limited}"
    );
}

#[test]
fn incremental_reverse_previous_is_the_child_and_the_newest_commit_has_none() {
    let (repo, home) = fixture("blame-previous-reverse");
    let c1 = rev_parse(&repo, &home, "HEAD~3");
    let c2 = rev_parse(&repo, &home, "HEAD~2");
    let c3 = rev_parse(&repo, &home, "HEAD~1");
    let c4 = rev_parse(&repo, &home, "HEAD");

    let out = git(
        &repo,
        &home,
        &["blame", "--incremental", "--reverse", "HEAD~2..HEAD", "g.txt"],
    );

    // Walking forwards the scapegoat is a child, so a `previous` here names a *newer*
    // commit. c3's entry points at c4, which is the whole point of `--reverse`.
    assert_eq!(
        path_blocks(&out, &c3),
        vec![vec![format!("previous {c4} g.txt"), "filename g.txt".to_string()]],
        "under --reverse, c3's scapegoat is its child c4:\n{out}"
    );
    // c4 is the newest commit in the range: it has no child to hand entries to, so
    // `first_scapegoat()` yields nothing and no `previous` is printed. A field derived
    // from `c4^` would wrongly name c3 here.
    assert_eq!(
        path_blocks(&out, &c4),
        vec![vec!["filename g.txt".to_string()]],
        "the newest commit under --reverse has no scapegoat:\n{out}"
    );
    // Every `previous` here points *forward*: c2's names c3, c3's names c4.
    assert_eq!(
        path_blocks(&out, &c2),
        vec![vec![format!("previous {c3} g.txt"), "filename g.txt".to_string()]],
        "under --reverse, c2's scapegoat is its child c3:\n{out}"
    );
    assert!(
        !out.contains(&format!("previous {c1}")) && !out.contains(&format!("previous {c2}")),
        "no entry may point backwards under --reverse:\n{out}"
    );
}

#[test]
fn an_unparsable_line_range_prints_the_bare_synopsis_and_nothing_else() {
    let (repo, home) = fixture("blame-range-usage");

    for spec in ["xyz", "1,x", "'^/one/,+2'"] {
        let out = run(&repo, &home, &["blame", "-L", spec, "g.txt"]);
        assert_eq!(out.status.code(), Some(129), "-L {spec} exit status");
        assert!(
            out.stdout.is_empty(),
            "-L {spec} must write nothing to stdout: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "usage: git blame [<options>] [<rev-opts>] [<rev>] [--] <file>\n",
            "-L {spec} must print the bare synopsis, not the option block"
        );
    }

    // `cmd_blame` picks the synopsis by the name it was invoked under
    // (`str_usage = cmd_is_annotate ? annotate_usage : blame_usage`).
    let annotate = run(&repo, &home, &["annotate", "-L", "xyz", "g.txt"]);
    assert_eq!(annotate.status.code(), Some(129));
    assert_eq!(
        String::from_utf8_lossy(&annotate.stderr),
        "usage: git annotate [<options>] [<rev-opts>] [<rev>] [--] <file>\n"
    );

    // A *structurally* invalid command line is the other usage path and still gets the
    // full option block, so the two must not be collapsed into one.
    let no_operand = run(&repo, &home, &["blame"]);
    assert_eq!(no_operand.status.code(), Some(129));
    let text = String::from_utf8_lossy(&no_operand.stderr);
    assert!(
        text.starts_with("usage: git blame [<options>] [<rev-opts>] [<rev>] [--] <file>\n")
            && text.contains("--[no-]line-porcelain"),
        "a missing operand keeps the parse-options block:\n{text}"
    );
}
