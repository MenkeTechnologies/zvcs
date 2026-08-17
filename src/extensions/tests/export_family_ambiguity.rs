//! `fast-export`, `bundle`, `cherry` and `filter-branch`: how many times one
//! operand is resolved, and what order `fast-export --first-parent` emits in.
//!
//! Both halves are about the same thing — the difference between *deciding*
//! something and *reading* it a second time.
//!
//! `get_oid_basic()`'s first branch decodes a name of exactly `hexsz` hex digits
//! as an object id before any ref lookup, and warns when a ref answers to those
//! same 40 characters (`object-name.c:689-702`). The warning is therefore a
//! counter: it fires once per resolution, so the number of them a command prints
//! is a direct readout of how many times that command resolved the operand. A
//! port gets it wrong in both directions — silently, by resolving through a path
//! that never reaches `get_oid_basic()`, and loudly, by resolving one operand
//! twice where git resolves it once — and neither shows up in the command's real
//! output.
//!
//! `filter-branch` is the interesting one, because it is a **shell script**
//! (`$(git --exec-path)/git-filter-branch`) rather than a builtin: it hands
//! `"$@"` to four separate `git rev-parse` processes and then spends two to three
//! more processes on every commit it rewrites. Each of those is its own
//! `get_oid_basic()`, so the counts below are not "once per operand" but a map of
//! the script's process fan-out — which is exactly what a port that resolves
//! natively has no reason to reproduce unless it is told to.
//!
//! The ordering half is `fast-export --first-parent`. `cmd_fast_export()` sets
//! `revs.topo_order = 1` (builtin/fast-export.c:1377), which without a
//! commit-graph becomes `revs->limited = 1` (revision.c:3157-3158), so
//! `prepare_revision_walk()` runs `limit_list()` and then
//! `sort_in_topological_order()` (revision.c:4011-4015). `first_parent_only`
//! lives in the first of those and **not** in the second: `limit_list()` chooses
//! the commits, `sort_in_topological_order()` orders them over every parent link
//! there is (commit.c:945-1030). Ordering a first-parent selection as though the
//! second parents were absent puts a merge ahead of the side branch it merges,
//! and the stream then names a mark it has not emitted yet.
//!
//! The fixture is built with the binary under test, so the tests need nothing
//! installed; when the machine has a stock git each case is additionally run
//! against it and the two outputs compared, which is what catches an expectation
//! that is self-consistent but not git's.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The first line of `object_name_msg`, enough to tell the explanatory paragraph
/// apart from the `warning:` line it follows.
const ADVICE_FIRST_LINE: &str = "Git normally never creates a ref that ends with 40 hex characters";

/// A stock git to compare against, or `None` when the machine has no foreign git
/// installed.
///
/// Resolved explicitly rather than through `PATH`: on a machine where zvcs
/// shadows `git` a `PATH` lookup silently makes the oracle the thing under test.
fn stock_git() -> Option<String> {
    if let Ok(p) = std::env::var("ZVCS_STOCK_GIT") {
        return Path::new(&p).exists().then_some(p);
    }
    ["/usr/bin/git", "/opt/homebrew/bin/git", "/usr/local/bin/git"]
        .into_iter()
        .filter(|p| Path::new(p).exists())
        .filter_map(|p| Some((version_of(p)?, p.to_owned())))
        // The comparisons below are byte-for-byte, and the advice paragraph's
        // `git config set …` spelling is 2.46 and newer. An older git is a
        // different oracle, not a worse one, so a machine that only has one runs
        // the counts without it.
        .filter(|(v, _)| *v >= (2, 55, 0))
        .max()
        .map(|(_, p)| p)
}

/// `git version X.Y.Z` as a comparable tuple, or `None` when it will not answer.
fn version_of(bin: &str) -> Option<(u32, u32, u32)> {
    let out = Command::new(bin).arg("--version").env_clear().output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let rest = text.trim().strip_prefix("git version ")?;
    let mut parts = rest.split(['.', ' ', '-']).filter_map(|p| p.parse::<u32>().ok());
    Some((parts.next()?, parts.next().unwrap_or(0), parts.next().unwrap_or(0)))
}

/// A fixture repository plus the binary that built it.
///
/// ```text
/// c1 - c2 - c3 ---- m   (main, HEAD)
///        \         /
///         s1 -----      (side)
/// ```
///
/// `c3` and `s1` touch different files, so the merge is clean and every commit
/// is reachable from `main` — which is what lets one operand be both a revision
/// argument and the id of a commit the rewrite loop visits.
struct Repo {
    bin: String,
    dir: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn git(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .current_dir(&self.dir)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
            // `git-filter-branch` opens with a ten-second `sleep` and a warning
            // block unless this is set, on both sides.
            .env("FILTER_BRANCH_SQUELCH_WARNING", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2023-01-01 00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000");
        cmd.output().unwrap()
    }

    fn stderr(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.git(args).stderr).into_owned()
    }

    fn stdout(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.git(args).stdout).into_owned()
    }

    fn rev(&self, spec: &str) -> String {
        String::from_utf8_lossy(&self.git(&["rev-parse", spec]).stdout).trim().to_owned()
    }

    /// `git update-ref refs/heads/<40-hex> <id>` — the accident the warning is
    /// about, made on purpose.
    ///
    /// The ref is pointed at the commit its own name spells, so the two possible
    /// resolutions agree on the *answer* and only the warning count separates
    /// them. That is deliberate here: these tests are about how many times a
    /// name is resolved, not about which resolution wins.
    fn hex_ref(&self, spec: &str) -> String {
        let id = self.rev(spec);
        let full = format!("refs/heads/{id}");
        let out = self.git(&["update-ref", &full, &id]);
        assert!(out.status.success(), "update-ref {full}: {}", String::from_utf8_lossy(&out.stderr));
        id
    }

    fn commit(&self, file: &str, body: &str, msg: &str) {
        std::fs::write(self.dir.join(file), format!("{body}\n")).unwrap();
        assert!(self.git(&["add", file]).status.success(), "add {file}");
        let out = self.git(&["commit", "-q", "-m", msg]);
        assert!(out.status.success(), "commit {msg}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

fn fixture(bin: &str, tag: &str) -> Repo {
    let root =
        std::env::temp_dir().join(format!("zvcs-efa-{tag}-{}-{:p}", std::process::id(), &tag));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let dir = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let repo = Repo { bin: bin.to_owned(), dir, home };
    assert!(repo.git(&["init", "-q", "-b", "main", "."]).status.success(), "init");
    repo.commit("f.txt", "one", "c1");
    repo.commit("f.txt", "two", "c2");
    assert!(repo.git(&["checkout", "-q", "-b", "side"]).status.success(), "branch side");
    repo.commit("s.txt", "side", "s1");
    assert!(repo.git(&["checkout", "-q", "main"]).status.success(), "back to main");
    repo.commit("f.txt", "three", "c3");
    let out = repo.git(&["merge", "-q", "--no-ff", "-m", "m", "side"]);
    assert!(out.status.success(), "merge: {}", String::from_utf8_lossy(&out.stderr));
    repo
}

/// How many `warning: refname …` lines `text` holds.
fn warnings(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with("warning: refname ")).count()
}

/// Run one case in a fresh fixture under zvcs and — when the machine has one —
/// under stock git, asserting the two produce identical stderr.
///
/// `setup` receives the repo and returns the argv to run, so a case can mint the
/// 40-hex refs and read the ids it needs on each side independently: the ids are
/// identical across the two runs (the dates are pinned) but each side must build
/// its own repository, since `filter-branch` rewrites the one it is given.
fn both<F>(tag: &str, setup: F) -> String
where
    F: Fn(&Repo) -> Vec<String>,
{
    let zvcs = fixture(BIN, &format!("{tag}-zvcs"));
    let zargs = setup(&zvcs);
    let zerr = zvcs.stderr(&zargs.iter().map(String::as_str).collect::<Vec<_>>());

    if let Some(bin) = stock_git() {
        let stock = fixture(&bin, &format!("{tag}-stock"));
        let sargs = setup(&stock);
        assert_eq!(sargs, zargs, "{tag}: the two fixtures must produce the same command line");
        let serr = stock.stderr(&sargs.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(serr, zerr, "{tag}: stderr must match stock for {zargs:?}");
    }
    zerr
}

// ---------------------------------------------------------- fast-export -----

/// `setup_revisions()` reaches `get_oid_with_context()` once for each name it
/// takes off the command line, so a lone operand is resolved exactly once —
/// however it is spelled. The suffix forms are here because
/// `get_oid_with_context_1()`, `get_oid_1()` and `peel_onion()` each cut their
/// suffix off *before* `get_oid_basic()` sees the name, so all three warn about
/// the same 40 characters and none of them warns twice.
#[test]
fn fast_export_warns_once_per_operand() {
    for (tag, spell) in
        [("plain", "{}"), ("caret", "^{}"), ("peel", "{}^{commit}"), ("tilde", "{}~1")]
    {
        let err = both(&format!("fe-one-{tag}"), |r| {
            let id = r.hex_ref("main~1");
            let arg = spell.replace("{}", &id);
            if tag == "caret" {
                vec!["fast-export".into(), arg, "main".into()]
            } else {
                vec!["fast-export".into(), arg]
            }
        });
        assert_eq!(warnings(&err), 1, "fast-export {spell} warns once:\n{err}");
    }
}

/// `handle_dotdot_1()` resolves a range's two endpoints in one `||`-joined pair
/// *before* either object is looked up, so both warn even when the left one
/// names an object the repository does not have — and a left endpoint that does
/// not resolve at all short-circuits the right one out of existence.
#[test]
fn fast_export_range_warns_per_endpoint_with_the_short_circuit() {
    // Left endpoint present, right endpoint not a 40-hex name: one warning.
    let one = both("fe-range-left", |r| {
        let id = r.hex_ref("main~1");
        vec!["fast-export".into(), format!("{id}..main")]
    });
    assert_eq!(warnings(&one), 1, "only the left endpoint is ambiguous:\n{one}");

    // Both endpoints ambiguous: two warnings, from the one pair of calls.
    let two = both("fe-range-both", |r| {
        let a = r.hex_ref("main~2");
        let b = r.hex_ref("main~1");
        vec!["fast-export".into(), format!("{a}..{b}")]
    });
    assert_eq!(warnings(&two), 2, "both endpoints resolve, so both warn:\n{two}");

    // A left endpoint that does not resolve at all: `||` short-circuits and the
    // right one is never looked at.
    let none = both("fe-range-short", |r| {
        let id = r.hex_ref("main~1");
        vec!["fast-export".into(), format!("nosuch..{id}")]
    });
    assert_eq!(warnings(&none), 0, "a failing left endpoint hides the right one:\n{none}");
}

// --------------------------------------------------------------- bundle -----

/// `cmd_bundle_create()` hands its operands to `setup_revisions()` too
/// (bundle.c:501), so the same rule holds — including for the range, whose two
/// endpoints are one `handle_dotdot_1()` pair and not two independent
/// resolutions.
#[test]
fn bundle_create_warns_once_per_resolution() {
    let one = both("bundle-one", |r| {
        let id = r.hex_ref("main~1");
        vec!["bundle".into(), "create".into(), "b.bdl".into(), id]
    });
    assert_eq!(warnings(&one), 1, "bundle create <40-hex-ref> warns once:\n{one}");

    let two = both("bundle-range", |r| {
        let a = r.hex_ref("main~2");
        let b = r.hex_ref("main~1");
        vec!["bundle".into(), "create".into(), "b.bdl".into(), format!("{a}..{b}")]
    });
    assert_eq!(warnings(&two), 2, "a range warns once per endpoint:\n{two}");

    // Two mentions of the same name are two operands, so two resolutions.
    let dup = both("bundle-dup", |r| {
        let id = r.hex_ref("main~1");
        vec!["bundle".into(), "create".into(), "b.bdl".into(), id.clone(), id]
    });
    assert_eq!(warnings(&dup), 2, "the same name twice is resolved twice:\n{dup}");
}

// --------------------------------------------------------------- cherry -----

/// `cmd_cherry()` calls `repo_get_oid()` once per operand — upstream, head, and
/// the optional limit — so each earns exactly one warning and never a second
/// from the walk that follows.
#[test]
fn cherry_warns_once_per_operand() {
    let one = both("cherry-one", |r| {
        let id = r.hex_ref("main~1");
        vec!["cherry".into(), id, "main".into()]
    });
    assert_eq!(warnings(&one), 1, "cherry warns once for its upstream:\n{one}");

    let three = both("cherry-three", |r| {
        let up = r.hex_ref("main~2");
        let head = r.hex_ref("side");
        let limit = r.hex_ref("main~1");
        vec!["cherry".into(), up, head, limit]
    });
    assert_eq!(warnings(&three), 3, "one warning per operand, no more:\n{three}");
}

// -------------------------------------------------------- filter-branch -----

/// `git-filter-branch` is a shell script, and its revision arguments go through
/// **four** separate `git rev-parse` processes: `--symbolic-full-name` (line
/// 269), `--no-revs` (316), `--revs-only` (325) and `--sq --no-revs` (329). Each
/// is its own `get_oid_basic()`.
///
/// The rewrite loop then spends two more on each commit it rewrites —
/// `git cat-file commit "$commit"` (417) and `tree=$(git rev-parse
/// "$commit^{tree}")` (478) — so an operand that is *also* the id of a rewritten
/// commit is resolved six times in all.
#[test]
fn filter_branch_warns_four_times_per_argument_and_twice_per_rewritten_commit() {
    // `main~1` is both the revision argument and a commit the walk rewrites.
    let six = both("fb-six", |r| {
        let id = r.hex_ref("main~1");
        vec!["filter-branch".into(), "--force".into(), id]
    });
    assert_eq!(warnings(&six), 6, "four from argv, two from the rewrite loop:\n{six}");

    // Excluded from the walk, so only the four argument passes remain — and the
    // fifth is `git commit-tree -p <id>` naming it as a parent of the merge.
    let five = both("fb-excluded", |r| {
        let id = r.hex_ref("main~1");
        vec!["filter-branch".into(), "--force".into(), format!("^{id}"), "main".into()]
    });
    assert_eq!(warnings(&five), 5, "four from argv plus one `commit-tree -p`:\n{five}");

    // Two mentions of the same name are two revision words in each of the four
    // passes, and the rewrite loop still visits the commit once.
    let ten = both("fb-dup", |r| {
        let id = r.hex_ref("main~1");
        vec!["filter-branch".into(), "--force".into(), id.clone(), id]
    });
    assert_eq!(warnings(&ten), 10, "eight from argv, two from the rewrite loop:\n{ten}");
}

/// The `heads` check (`test -s "$tempdir"/heads`, line 286) sits between the
/// first `rev-parse` pass and the other three, so where the run stops decides how
/// many of the four passes happen at all.
///
/// `--symbolic-full-name` answers with a ref or with nothing, and the loop that
/// reads it skips the `^`-marked lines (`case "$ref" in ^?*) continue`). So a
/// range whose only ref-named endpoint is the excluded one leaves `heads` empty,
/// and the script dies with the *first* pass's warnings and no others.
#[test]
fn filter_branch_stops_between_the_first_pass_and_the_rest() {
    let err = both("fb-range", |r| {
        let a = r.hex_ref("main~2");
        // The merge itself: a full-length hex with no ref of that name, so
        // `--symbolic-full-name` prints nothing for it.
        let b = r.rev("main");
        vec!["filter-branch".into(), "--force".into(), format!("{a}..{b}")]
    });
    assert_eq!(warnings(&err), 1, "one pass, one ambiguous endpoint, then the die:\n{err}");
    assert!(err.contains("You must specify a ref to rewrite."), "and that is the die:\n{err}");
}

/// `$need_index` (lines 376-382) adds `git read-tree -i -m $commit` to the top of
/// the loop, and takes `git rev-parse "$commit^{tree}"` off the bottom — the
/// tree comes from `git write-tree` instead. `--index-filter` therefore still
/// costs six, while `--tree-filter` costs seven: it keeps the `read-tree` and
/// adds `git diff-index … $commit --` (line 434).
#[test]
fn filter_branch_need_index_moves_the_per_commit_resolutions() {
    let index = both("fb-index", |r| {
        let id = r.hex_ref("main~1");
        vec!["filter-branch".into(), "--force".into(), "--index-filter".into(), "true".into(), id]
    });
    assert_eq!(warnings(&index), 6, "read-tree + cat-file, no `^{{tree}}`:\n{index}");

    let tree = both("fb-tree", |r| {
        let id = r.hex_ref("main~1");
        vec!["filter-branch".into(), "--force".into(), "--tree-filter".into(), "true".into(), id]
    });
    assert_eq!(warnings(&tree), 7, "read-tree + cat-file + diff-index:\n{tree}");
}

/// The `--subdirectory-filter` arm of that same `case` is
///
/// ```sh
/// err=$(GIT_ALLOW_NULL_SHA1=1 \
///       git read-tree -i -m $commit:"$filter_subdir" 2>&1) || { … }
/// ```
///
/// and the `2>&1` inside a command substitution puts `read-tree`'s whole stderr
/// — warning included — into `$err`, which the success path throws away. So it
/// prints one fewer than `--index-filter` on the same commit, which is a
/// property of the *redirection* and not of the resolution.
#[test]
fn filter_branch_subdirectory_filter_swallows_its_read_tree_warning() {
    let err = both("fb-subdir", |r| {
        std::fs::create_dir_all(r.dir.join("sub")).unwrap();
        r.commit("sub/x.txt", "x", "sub");
        let id = r.hex_ref("main");
        vec![
            "filter-branch".into(),
            "--force".into(),
            "--subdirectory-filter".into(),
            "sub".into(),
            id,
        ]
    });
    assert_eq!(warnings(&err), 5, "four from argv, one from `cat-file commit`:\n{err}");
}

// ------------------------------------------------- must stay silent ---------

/// `builtin/update-ref.c` passes `GET_OID_SKIP_AMBIGUITY_CHECK`, which is the
/// `!(flags & …)` in the same condition — so it never consults the switch at all
/// and writes a 40-hex-named ref without a word about it.
#[test]
fn update_ref_never_warns() {
    let err = both("silent-update-ref", |r| {
        let id = r.hex_ref("main~1");
        vec!["update-ref".into(), "refs/heads/probe".into(), id]
    });
    assert_eq!(warnings(&err), 0, "update-ref passes GET_OID_SKIP_AMBIGUITY_CHECK:\n{err}");
}

/// `get_object_list()` (builtin/pack-objects.c) is one of the four places git
/// clears `warn_on_object_refname_ambiguity` by hand around a bulk read, so the
/// `--revs` stdin loop is silent however many 40-hex names it is fed.
#[test]
fn pack_objects_revs_never_warns() {
    let run = |bin: &str, tag: &str| -> (String, usize) {
        let r = fixture(bin, tag);
        let id = r.hex_ref("main~1");
        let mut cmd = Command::new(bin);
        cmd.args(["pack-objects", "--revs", "--stdout"])
            .current_dir(&r.dir)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &r.home)
            .env("ZVCS_HOME", &r.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(format!("{id}\n").as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        let n = warnings(&err);
        (err, n)
    };
    let (zerr, zn) = run(BIN, "silent-pack-zvcs");
    assert_eq!(zn, 0, "pack-objects --revs clears the switch:\n{zerr}");
    if let Some(bin) = stock_git() {
        let (_, sn) = run(&bin, "silent-pack-stock");
        assert_eq!(sn, zn, "pack-objects --revs must match stock");
    }
}

// ----------------------------------------------------------- the gates ------

/// `core.warnAmbiguousRefs` is the third of `get_oid_basic()`'s four conditions
/// and defaults to true, so silence takes setting it false — and then it is
/// total, paragraph included, on every verb here.
#[test]
fn warn_ambiguous_refs_false_silences_every_verb() {
    for (tag, argv) in [
        ("fe", vec!["fast-export"]),
        ("cherry", vec!["cherry"]),
        ("fb", vec!["filter-branch", "--force"]),
    ] {
        let err = both(&format!("gate-core-{tag}"), |r| {
            let id = r.hex_ref("main~1");
            r.git(&["config", "core.warnAmbiguousRefs", "false"]);
            let mut v: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
            v.push(id);
            if tag == "cherry" {
                v.push("main".into());
            }
            v
        });
        assert_eq!(warnings(&err), 0, "{tag}: core.warnAmbiguousRefs=false silences it:\n{err}");
        assert!(!err.contains(ADVICE_FIRST_LINE), "{tag}: and the paragraph with it:\n{err}");
    }
}

/// `advice.objectNameWarning` gates only the explanatory paragraph. The
/// `warning:` line is printed with a bare `fprintf(stderr, …)` rather than
/// through `advise()`, so every one of the six survives.
#[test]
fn object_name_warning_false_keeps_the_warning_lines() {
    let err = both("gate-advice", |r| {
        let id = r.hex_ref("main~1");
        r.git(&["config", "advice.objectNameWarning", "false"]);
        vec!["filter-branch".into(), "--force".into(), id]
    });
    assert_eq!(warnings(&err), 6, "the warning line is not advice:\n{err}");
    assert!(!err.contains(ADVICE_FIRST_LINE), "but the paragraph is:\n{err}");
}

/// No ref by that name, no warning: the message is about a ref created by
/// accident, not about the id. This is the control that keeps every count above
/// from being satisfied by a port that simply warns whenever it sees 40 hex
/// digits.
#[test]
fn without_a_ref_of_that_name_every_verb_is_silent() {
    for (tag, argv) in [
        ("fe", vec!["fast-export"]),
        ("bundle", vec!["bundle", "create", "b.bdl"]),
        ("cherry", vec!["cherry"]),
        ("fb", vec!["filter-branch", "--force"]),
    ] {
        let err = both(&format!("noref-{tag}"), |r| {
            let id = r.rev("main~1");
            let mut v: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
            v.push(id);
            if tag == "cherry" {
                v.push("main".into());
            }
            v
        });
        assert_eq!(warnings(&err), 0, "{tag}: a 40-hex operand with no such ref is silent:\n{err}");
    }
}

// ------------------------------------------ fast-export --first-parent ------

/// The `commit <ref>` lines of a fast-export stream, in stream order.
fn commit_refs(stream: &str) -> Vec<&str> {
    stream.lines().filter_map(|l| l.strip_prefix("commit ")).collect()
}

/// Every `from :<n>` / `merge :<n>` reference points at a mark the stream has
/// already defined.
///
/// This is the property the ordering bug broke: with the side branch emitted
/// *after* the merge that joins it, there is no mark to name and `fast-export`
/// silently drops the `merge` line, turning a merge commit into an ordinary one.
fn marks_are_defined_before_use(stream: &str) -> Result<(), String> {
    let mut defined = std::collections::HashSet::new();
    for line in stream.lines() {
        if let Some(n) = line.strip_prefix("mark :") {
            defined.insert(n.to_owned());
        }
        for prefix in ["from :", "merge :"] {
            if let Some(n) = line.strip_prefix(prefix) {
                if !defined.contains(n) {
                    return Err(format!("{line}: mark :{n} used before it was defined"));
                }
            }
        }
    }
    Ok(())
}

/// `sort_in_topological_order()` has no `first_parent_only` test in it: it
/// counts in-degrees and enqueues parents over the *whole* parent list, because
/// `limit_list()` has already decided which commits are in the list. So
/// `--first-parent` changes the membership and not the order, and the merge is
/// still emitted after the side branch it merges.
#[test]
fn fast_export_first_parent_emits_the_side_branch_before_the_merge() {
    for args in [
        vec!["fast-export", "--all", "--first-parent"],
        vec!["fast-export", "--first-parent", "main", "side"],
        vec!["fast-export", "--first-parent", "side", "main"],
        vec!["fast-export", "--first-parent", "--date-order", "--all"],
    ] {
        let zvcs = fixture(BIN, &format!("fp-{}", args.join("-")));
        let stream = zvcs.stdout(&args);
        marks_are_defined_before_use(&stream).unwrap_or_else(|e| panic!("{args:?}: {e}\n{stream}"));

        // Five commits, and the last one is the merge — which is the whole
        // point: the side branch it joins has to have been emitted already for
        // the `merge :<n>` line above to name anything.
        let refs = commit_refs(&stream);
        assert_eq!(refs.len(), 5, "{args:?}: five commits in the stream:\n{stream}");
        assert_eq!(refs[4], "refs/heads/main", "{args:?}: the merge is last:\n{stream}");
        assert_eq!(
            stream.matches("\nmerge :").count(),
            1,
            "{args:?}: the merge keeps its second parent:\n{stream}"
        );

        if let Some(bin) = stock_git() {
            let stock = fixture(&bin, &format!("fp-stock-{}", args.join("-")));
            assert_eq!(stock.stdout(&args), stream, "{args:?}: the whole stream must match stock");
        }
    }
}

/// The stream `--first-parent` produces has to be one `fast-import` accepts and
/// rebuild the same graph — the ordering bug produced a stream that imported
/// *cleanly* and quietly lost the merge, which no exit status would have caught.
#[test]
fn fast_export_first_parent_stream_round_trips() {
    let src = fixture(BIN, "fp-rt-src");
    let stream = src.stdout(&["fast-export", "--all", "--first-parent"]);
    assert!(!stream.is_empty(), "fast-export produced nothing");

    let dst = std::env::temp_dir().join(format!("zvcs-efa-fp-rt-dst-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::create_dir_all(&dst).unwrap();
    let target = Repo { bin: BIN.to_owned(), dir: dst, home: src.home.clone() };
    assert!(target.git(&["init", "-q", "-b", "main", "."]).status.success(), "init target");

    let mut cmd = Command::new(BIN);
    cmd.args(["fast-import", "--quiet"])
        .current_dir(&target.dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &target.home)
        .env("ZVCS_HOME", &target.home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", "2023-01-01 00:00:00 +0000")
        .stdin(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "fast-import: {}", String::from_utf8_lossy(&out.stderr));

    // The imported history must be the same shape, merge included.
    let shape = |r: &Repo| r.stdout(&["log", "--all", "--format=%s %P", "--topo-order"]);
    assert_eq!(shape(&target), shape(&src), "the round trip must rebuild the same graph");
}
