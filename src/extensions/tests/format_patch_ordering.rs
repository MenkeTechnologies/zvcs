//! `format-patch`'s revision-walk ordering options.
//!
//! `--topo-order`, `--date-order`, `--author-date-order`, `--no-walk` and
//! `--reverse` are not format-patch options at all — they belong to
//! `setup_revisions()`, which format-patch shares with `log` and `rev-list`. What
//! makes them worth their own file is that `cmd_format_patch` collects the whole
//! walk into `list[]` and then emits it *backwards* (`while (0 <= --nr)`), so
//! every ordering below is the reverse of the order `git log` would print, and
//! `--reverse` — which flips the walk itself inside `get_revision()` — cancels
//! that second flip rather than adding to it.
//!
//! The fixture is shaped so the three orderings disagree: two side branches whose
//! commit dates interleave (which is what `--date-order` follows) while their
//! author dates interleave the other way (`--author-date-order`), and neither
//! matches the contiguous-branch order `--topo-order` produces.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

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
    /// ```text
    ///   main:  A --- B --- C ------- M
    ///           \                   /
    ///   b2:      \--- D --- E ------/
    /// ```
    /// with commit dates A=05 B=10 D=20 E=30 C=40 M=50 and author dates reversed
    /// across the two branches: A=05 C=10 E=20 D=30 B=40 M=50.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-fporder-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.email", "t@e.co"]);
        f.git(&["config", "user.name", "t"]);
        f.commit("A", 5, 5);
        f.git(&["branch", "b2"]);
        f.commit("B", 10, 40);
        f.commit("C", 40, 10);
        f.git(&["checkout", "-q", "b2"]);
        f.commit("D", 20, 30);
        f.commit("E", 30, 20);
        f.git(&["checkout", "-q", "main"]);
        f.at(&["merge", "-q", "--no-ff", "-m", "M", "b2"], 50, 50);
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
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL")
            .env_remove("GIT_AUTHOR_DATE")
            .env_remove("GIT_COMMITTER_DATE");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    /// Run a setup command with both timestamps pinned to a second of 2020-01-01.
    fn at(&self, args: &[&str], commit_sec: u32, author_sec: u32) {
        let out = self
            .cmd(args)
            .env("GIT_COMMITTER_DATE", stamp(commit_sec))
            .env("GIT_AUTHOR_DATE", stamp(author_sec))
            .output()
            .unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn commit(&self, name: &str, commit_sec: u32, author_sec: u32) {
        std::fs::write(self.work.join(format!("{name}.txt")), format!("{name}\n")).unwrap();
        self.git(&["add", "-A"]);
        self.at(&["commit", "-q", "-m", name], commit_sec, author_sec);
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn text(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The object id off the cover letter's magic `From <oid>` line.
    fn cover_from(&self, args: &[&str]) -> String {
        let body = self.text(args);
        let line = body.lines().next().expect("a cover letter");
        line.split_whitespace().nth(1).expect("an oid").to_owned()
    }

    fn rev(&self, spec: &str) -> String {
        self.text(&["rev-parse", spec]).trim().to_owned()
    }

    /// The ` name | n +` rows of the cover letter's combined diffstat, which is
    /// the block between the shortlog and the signature.
    fn cover_stat_files(&self, args: &[&str]) -> Vec<String> {
        let body = self.text(args);
        let cover = body.split("\nFrom ").next().expect("a cover letter");
        cover
            .lines()
            .filter_map(|l| l.split_once(" | "))
            .map(|(name, _)| name.trim().to_owned())
            .collect()
    }

    /// The `Subject:` line of every patch the invocation emitted, in order, with
    /// the `[PATCH n/m]` bracket dropped so only the series order is asserted.
    fn subjects(&self, args: &[&str]) -> Vec<String> {
        let out = self.run(args);
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix("Subject: "))
            .map(|s| s.rsplit("] ").next().unwrap_or(s).to_owned())
            .collect()
    }
}

fn stamp(sec: u32) -> String {
    format!("2020-01-01T00:00:{sec:02} +0000")
}

/// Each ordering flag walks the two branches differently, and format-patch emits
/// the reverse of what it walked.
///
///   * default — the commit-date priority queue: C(40) E(30) D(20) B(10) A(05),
///     reversed to A B D E C.
///   * `--date-order` — `sort_in_topological_order()` with the same commit-date
///     tie-break, which on this shape agrees with the default.
///   * `--topo-order` — a LIFO stack, so a branch stays contiguous: A B C D E.
///   * `--author-date-order` — the author dates put E(20) before D(30) and C(10)
///     before B(40), which reverses to the same A B C D E here for a different
///     reason: the tie-break is read off the `author` line, not the `committer`.
#[test]
fn ordering_flags_reshuffle_the_series() {
    let f = Fixture::new("orders");
    let base: &[&str] = &["format-patch", "--stdout", "--root", "HEAD"];

    assert_eq!(f.subjects(base), ["A", "B", "D", "E", "C"]);
    assert_eq!(
        f.subjects(&[base, &["--date-order"]].concat()),
        ["A", "B", "D", "E", "C"]
    );
    assert_eq!(
        f.subjects(&[base, &["--topo-order"]].concat()),
        ["A", "B", "C", "D", "E"]
    );
    assert_eq!(
        f.subjects(&[base, &["--author-date-order"]].concat()),
        ["A", "B", "C", "D", "E"]
    );

    // The merge is never formatted, whichever order asked for it.
    for order in ["--topo-order", "--date-order", "--author-date-order"] {
        assert!(
            !f.subjects(&[base, &[order]].concat()).contains(&"M".to_owned()),
            "{order} formatted the merge"
        );
    }
}

/// `--reverse` flips the walk inside `get_revision()`, which cancels the flip
/// `cmd_format_patch` applies when it emits `list[]` backwards — so the series
/// comes out newest-first, in whatever order the ordering flag walked.
#[test]
fn reverse_cancels_the_emission_flip() {
    let f = Fixture::new("reverse");
    let base: &[&str] = &["format-patch", "--stdout", "--root", "--reverse", "HEAD"];

    assert_eq!(f.subjects(base), ["C", "E", "D", "B", "A"]);
    assert_eq!(
        f.subjects(&[base, &["--date-order"]].concat()),
        ["C", "E", "D", "B", "A"]
    );
    assert_eq!(
        f.subjects(&[base, &["--topo-order"]].concat()),
        ["E", "D", "C", "B", "A"]
    );
    assert_eq!(
        f.subjects(&[base, &["--author-date-order"]].concat()),
        ["E", "D", "C", "B", "A"]
    );
}

/// The ordering runs over the whole walk, before `--skip` and `-<n>` cut it down —
/// so the counts see the reshuffled list, not the date-ordered one.
#[test]
fn ordering_precedes_skip_and_count() {
    let f = Fixture::new("counts");
    let base: &[&str] = &["format-patch", "--stdout", "--root", "HEAD"];

    // `--topo-order` walks E D C B A; `-2` keeps E D, which emits as D E.
    assert_eq!(f.subjects(&[base, &["--topo-order", "-2"]].concat()), ["D", "E"]);
    // The default walk is C E D B A, so the same `-2` keeps C E.
    assert_eq!(f.subjects(&[base, &["-2"]].concat()), ["E", "C"]);
    // `--skip` drops from the head of the walk, which the emission flip then
    // turns into the tail of the printed series.
    assert_eq!(
        f.subjects(&[base, &["--topo-order", "--skip=1"]].concat()),
        ["A", "B", "C", "D"]
    );
    assert_eq!(
        f.subjects(&[base, &["--skip=1"]].concat()),
        ["A", "B", "D", "E"]
    );
}

/// `--no-walk` lists the named commits and never reaches their parents. It is
/// positional: `-<n>`, `--max-count`, `--do-walk` and any UNINTERESTING endpoint
/// all clear it again, and a later `--no-walk` turns it back on.
#[test]
fn no_walk_lists_only_the_named_commits() {
    let f = Fixture::new("nowalk");

    // Two named tips, sorted by commit date and then emitted backwards. C(40)
    // walks before E(30), so E comes out first.
    assert_eq!(
        f.subjects(&["format-patch", "--stdout", "--no-walk", "--root", "HEAD~1", "b2"]),
        ["E", "C"]
    );
    // `--no-walk=unsorted` keeps the command-line order instead.
    assert_eq!(
        f.subjects(&["format-patch", "--stdout", "--no-walk=unsorted", "--root", "b2", "HEAD~1"]),
        ["C", "E"]
    );
    // HEAD alone is the merge, which format-patch never emits.
    assert!(f
        .subjects(&["format-patch", "--stdout", "--no-walk", "--root", "HEAD"])
        .is_empty());

    // A range queues an UNINTERESTING endpoint, and `add_pending_object()` clears
    // `no_walk` when it does — so this walks after all.
    assert_eq!(
        f.subjects(&["format-patch", "--stdout", "--no-walk", "main~1..main"]),
        ["D", "E"]
    );
    // …but a `--no-walk` that comes *after* the range switches it back on, and
    // the one remaining tip is the merge.
    assert!(f
        .subjects(&["format-patch", "--stdout", "main~1..main", "--no-walk"])
        .is_empty());

    // `-<n>` clears it too, and is likewise positional.
    assert_eq!(
        f.subjects(&["format-patch", "--stdout", "--no-walk", "-2", "HEAD"]),
        ["E", "C"]
    );
    assert!(f
        .subjects(&["format-patch", "--stdout", "-2", "--no-walk", "HEAD"])
        .is_empty());
    // `--do-walk` is the explicit off switch.
    assert_eq!(
        f.subjects(&["format-patch", "--stdout", "--no-walk", "--do-walk", "--root", "HEAD"]),
        ["A", "B", "D", "E", "C"]
    );

    // `prepare_revision_walk()` returns under `--no-walk` *before* it reaches
    // `sort_in_topological_order()`, so the ordering flags are silently inert and
    // the plain commit-date sort of the pending list is what survives.
    for order in ["--topo-order", "--date-order", "--author-date-order"] {
        assert_eq!(
            f.subjects(&[
                "format-patch",
                "--stdout",
                order,
                "--no-walk",
                "--root",
                "b2",
                "main~1",
                "HEAD",
            ]),
            ["E", "C"],
            "{order} reordered a --no-walk list"
        );
    }
}

/// An unrecognised `--no-walk=<value>` is reported by
/// `handle_revision_pseudo_opt()` and then falls through to the unknown-option
/// list, so git prints both lines and exits 128.
#[test]
fn no_walk_rejects_an_unknown_argument() {
    let f = Fixture::new("nowalkbad");
    let out = f.run(&["format-patch", "--stdout", "--no-walk=bogus", "--root", "HEAD"]);
    assert_eq!(out.status.code(), Some(128), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: invalid argument to --no-walk\nfatal: unrecognized argument: --no-walk=bogus\n"
    );
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// The cover letter is built from `head = list[0]`, the first commit the walk
/// handed back — which is the last patch of the printed series either way round,
/// because `--reverse` flips the walk and the emission together.
#[test]
fn cover_letter_names_the_first_walked_commit() {
    let f = Fixture::new("coverhead");
    let (a, c) = (f.rev("main~1~1~1"), f.rev("main~1"));

    let forward = &["format-patch", "--stdout", "--root", "--cover-letter", "HEAD"];
    // Forward: the walk starts at C, and C is the last patch printed.
    assert_eq!(f.cover_from(forward), c);
    assert_eq!(f.subjects(forward).last().unwrap(), "C");

    let reversed = &[
        "format-patch",
        "--stdout",
        "--root",
        "--reverse",
        "--cover-letter",
        "HEAD",
    ];
    // Reversed: the walk starts at A, and A is now the last patch printed.
    assert_eq!(f.cover_from(reversed), a);
    assert_eq!(f.subjects(reversed).last().unwrap(), "A");
}

/// The cover letter's combined diffstat needs "a unique reference point": the
/// single boundary commit of the walk. A series with none — every root reached —
/// or with more than one gets no stat block at all.
#[test]
fn cover_letter_diffstat_follows_the_boundary_commit() {
    let f = Fixture::new("coverstat");

    // One boundary (C, the excluded side of the range): the stat is C..E.
    assert_eq!(
        f.cover_stat_files(&["format-patch", "--stdout", "--cover-letter", "main~1..main"]),
        ["D.txt", "E.txt"]
    );
    // `--root` walks past every boundary, so there is no reference point left.
    assert!(f
        .cover_stat_files(&["format-patch", "--stdout", "--root", "--cover-letter", "HEAD"])
        .is_empty());
    // Two named tips with two different parents: two boundaries, so no stat.
    assert!(f
        .cover_stat_files(&[
            "format-patch",
            "--stdout",
            "--no-walk",
            "--root",
            "--cover-letter",
            "b2",
            "main~1",
        ])
        .is_empty());
    // `--no-walk` leaves the boundary commit unparsed, and git then diffs against
    // a NULL tree — so the stat is everything in the tip's tree, not the range.
    assert_eq!(
        f.cover_stat_files(&[
            "format-patch",
            "--stdout",
            "--no-walk",
            "--root",
            "--cover-letter",
            "b2",
        ]),
        ["A.txt", "D.txt", "E.txt"]
    );
}

/// The `--no-walk` NULL-tree quirk above is about *parsing*, not about the flag:
/// a boundary commit that the revision arguments named themselves went through
/// `handle_commit()` and still has its tree, so its stat is the real range.
#[test]
fn a_named_boundary_keeps_its_tree_under_no_walk() {
    let f = Fixture::new("nowalkparsed");
    // One more commit on top of the merge, so the series' boundary can be a
    // commit that is also on the pending list.
    f.commit("Z", 60, 60);

    // `HEAD` is Z and `main~1` is the merge: the merge is dropped from the series
    // but stays pending, and it is Z's only parent — a parsed boundary.
    assert_eq!(
        f.cover_stat_files(&[
            "format-patch",
            "--stdout",
            "--no-walk",
            "--root",
            "--cover-letter",
            "HEAD",
            "main~1",
        ]),
        ["Z.txt"]
    );
    // `main~2` is C, whose parent B was never named: an unparsed boundary, so the
    // stat is C's whole tree again.
    assert_eq!(
        f.cover_stat_files(&[
            "format-patch",
            "--stdout",
            "--no-walk",
            "--root",
            "--cover-letter",
            "main~2",
        ]),
        ["A.txt", "B.txt", "C.txt"]
    );
}

/// The regression this file was added for: an ordering flag alongside the rest of
/// a real invocation — a long `--add-header`, `-U0` and a `-v3` reroll, which
/// names the file `v3-0001-…` rather than `0001-…`.
#[test]
fn date_order_travels_with_the_other_options() {
    let f = Fixture::new("mixed");
    let out = f.run(&[
        "format-patch",
        "--add-header=99999999999999999999999999",
        "--date-order",
        "-U0",
        "--reroll-count=3",
        "HEAD~1",
    ]);
    assert!(out.status.success(), "{out:?}");
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    // `HEAD~1` is the merge's first parent, so the traditional `<since>..HEAD`
    // shorthand formats the other branch: D then E.
    assert_eq!(names, ["v3-0001-D.patch", "v3-0002-E.patch"], "{out:?}");

    let body = std::fs::read_to_string(f.work.join("v3-0001-D.patch")).unwrap();
    // The reroll count renames the file *and* the subject prefix.
    assert!(body.contains("Subject: [PATCH v3 1/2] D\n"), "{body}");
    // `--add-header` follows the headers git generates, verbatim and unwrapped
    // however long the value is.
    assert!(
        body.contains("Subject: [PATCH v3 1/2] D\n99999999999999999999999999\n"),
        "{body}"
    );
    // `-U0` leaves no context lines around the single added line.
    assert!(body.contains("@@ -0,0 +1 @@\n+D\n"), "{body}");
}
