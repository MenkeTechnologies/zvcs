//! `git fast-import`'s two end-of-run reports, against stock git.
//!
//! * The statistics block `cmd_fast_import()` prints on stderr unless `--quiet`
//!   turned it off (builtin/fast-import.c:4301-4326): object, duplicate, delta
//!   and delta-attempt counts per type, branches and branch loads, the mark
//!   table's span and fill, interned names, and the memory fast-import's pools
//!   allocated, then `pack_report()` (packfile.c:50-69). The counters follow
//!   from fast-import's own tree model — which trees it re-stores, which it
//!   tries to deltify against their old version, which subtrees it reads — so a
//!   port that writes the same objects can still print different numbers.
//! * The crash report `die_nicely()` writes to `$GIT_DIR/fast_import_crash_<pid>`
//!   and names on stderr (builtin/fast-import.c:353-463): the last commands, the
//!   active-branch LRU, every branch's loaded/dirty state and tree versions, the
//!   annotated tags and the marks.
//!
//! The process ids and the time in the report are normalised; nothing else is.
//! The `pack_report` window counters (`pack_used_ctr`, `pack_mmap_calls`,
//! `pack_open_windows`, `pack_mapped`) count `packfile.c`'s mmap windows, which
//! git opens whenever it reads an object back out of a pack — its own pack
//! included. The streams compared byte for byte here never read an object back
//! out of a pack; the ones that do are compared with those four lines set aside.

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// One side's run: stdout, stderr, exit code, and the crash report it left.
struct Run {
    stdout: Vec<u8>,
    stderr: String,
    code: i32,
    report: Option<String>,
}

/// A fresh repository under a per-test directory, so no run sees another's.
fn fresh(tag: &str, side: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-fi-report-{}-{tag}-{side}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = root.join("repo");
    let st = git(BIN, &repo, &root, &["init", "-q", "-b", "main"], None);
    assert_eq!(st.code, 0, "init failed");
    (root, repo)
}

fn git(bin: &str, cwd: &Path, root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Run {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env("HOME", root)
        .env("ZVCS_HOME", root.join("zvcs-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"))
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.invalid")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.unwrap_or_default()).unwrap();
    let out = child.wait_with_output().unwrap();
    Run {
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
        report: None,
    }
}

/// Run `args` with `stream` on stdin in a fresh repository for each binary,
/// from `subdir` of the work tree, and pick up any crash report.
fn both(tag: &str, subdir: &str, args: &[&str], stream: &[u8]) -> Option<(Run, Run)> {
    let stock = stock_git()?;
    let mut runs = Vec::new();
    for (side, bin) in [("stock", stock), ("zvcs", BIN)] {
        let (root, repo) = fresh(tag, side);
        let mut run = git(bin, &repo.join(subdir), &root, args, Some(stream));
        for entry in std::fs::read_dir(repo.join(".git")).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().starts_with("fast_import_crash_") {
                run.report = Some(std::fs::read_to_string(entry.path()).unwrap());
            }
        }
        runs.push(run);
    }
    let zvcs = runs.pop().unwrap();
    let stock = runs.pop().unwrap();
    Some((stock, zvcs))
}

/// The process ids and the time are the only parts of a report that differ
/// between two runs of the same stream.
fn normalise(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.starts_with("    fast-import process: ") || line.starts_with("    parent process     : ") {
                format!("{}N", &line[..25])
            } else if line.starts_with("    at ") {
                "    at T".to_string()
            } else if let Some(at) = line.find("fast_import_crash_") {
                format!("{}fast_import_crash_N", &line[..at])
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The stderr of a run with the four `pack_report` window counters removed.
fn without_window_counters(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|l| {
            !["pack_used_ctr", "pack_mmap_calls", "pack_open_windows", "pack_mapped"]
                .iter()
                .any(|c| l.starts_with(&format!("pack_report: {c}")))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn same(tag: &str, stock: &Run, zvcs: &Run) {
    assert_eq!(stock.code, zvcs.code, "{tag}: exit code\nstock: {}\nzvcs: {}", stock.stderr, zvcs.stderr);
    assert_eq!(
        String::from_utf8_lossy(&stock.stdout),
        String::from_utf8_lossy(&zvcs.stdout),
        "{tag}: stdout"
    );
}

/// Two commits on one branch and an annotated tag: two blobs, three trees
/// (the root twice, `b/` once) and the second root tree tried against the first
/// as a delta base. Nothing is read back out of the pack.
const LINEAR: &[u8] = b"blob\nmark :1\ndata 6\nhello\n\n\
commit refs/heads/main\nmark :2\ncommitter A <a@x> 1700000000 +0000\ndata 4\none\nM 100644 :1 a.txt\n\n\
blob\nmark :3\ndata 12\nhello world\n\n\
commit refs/heads/main\nmark :4\ncommitter A <a@x> 1700000001 +0000\ndata 4\ntwo\nfrom :2\n\
M 100644 :3 a.txt\nM 100644 :1 b/c.txt\n\n\
tag v1\nfrom :4\ntagger A <a@x> 1700000002 +0000\ndata 4\ntag\n\n";

/// Branches, copies, renames, deletes, notes, `deleteall`, `ls` and `cat-blob`
/// against the commit being built, a `checkpoint` and a `progress`: the paths
/// that load, dirty, store and release fast-import's in-memory trees.
const BRANCHY: &[u8] = b"feature done\n\
blob\nmark :1\ndata 30\nline one\nline two\nline three\n\n\
blob\nmark :2\ndata 40\nline one\nline two\nline three\nline four!\n\n\
commit refs/heads/a\nmark :3\ncommitter A <a@x> 1700000000 +0000\ndata 2\na\n\
M 100644 :1 x/y/z.txt\nM 100644 :2 x/w.txt\nM 755 :1 top\n\n\
progress after a\n\n\
commit refs/heads/b\nmark :4\ncommitter A <a@x> 1700000001 +0000\ndata 2\nb\nfrom :3\n\
C x x2\nR top top2\nD x/w.txt\n\n\
checkpoint\n\n\
commit refs/heads/c\nmark :5\ncommitter A <a@x> 1700000002 +0000\ndata 2\nc\nfrom refs/heads/a\n\
deleteall\nM 100644 inline new\ndata <<EOT\ninline body that is long enough to be deltified hopefully\nEOT\n\
ls \"new\"\ncat-blob :2\n\n\
commit refs/heads/a\nmark :6\ncommitter A <a@x> 1700000003 +0000\ndata 2\nd\nfrom :4\n\
N :1 :3\nN inline :4\ndata 5\nnote\n\nls :4 x2\n\n\
reset refs/tags/t\nfrom :6\n\n\
tag v1\nfrom :6\ntagger A <a@x> 1700000004 +0000\ndata 3\nv1\n\n\
get-mark :6\n";

/// The terminator [`BRANCHY`] declares it needs.
const DONE: &[u8] = b"done\n";

#[test]
fn statistics_block_matches_byte_for_byte() {
    for args in [
        &["fast-import"][..],
        &["fast-import", "--stats"],
        &["fast-import", "--quiet", "--stats"],
        &["fast-import", "--depth=0"],
        &["fast-import", "--big-file-threshold=4"],
        &["fast-import", "--active-branches=1"],
    ] {
        let Some((stock, zvcs)) = both("linear", "", args, LINEAR) else { return };
        same("linear", &stock, &zvcs);
        assert!(stock.stderr.starts_with("fast-import statistics:\n"), "{args:?}: {}", stock.stderr);
        assert_eq!(stock.stderr, zvcs.stderr, "{args:?}: the statistics block");
    }
}

#[test]
fn quiet_turns_the_block_off_and_stats_turns_it_back_on() {
    for (args, stream) in [
        (&["fast-import", "--quiet"][..], LINEAR),
        (&["fast-import", "--stats", "--quiet"][..], LINEAR),
    ] {
        let Some((stock, zvcs)) = both("quiet", "", args, stream) else { return };
        same("quiet", &stock, &zvcs);
        assert_eq!(stock.stderr, "", "{args:?}");
        assert_eq!(stock.stderr, zvcs.stderr, "{args:?}");
    }
    // `option git quiet` from the stream, and `option git stats` after it.
    for (prefix, expect_block) in [(&b"option git quiet\n"[..], false), (&b"option git quiet\noption git stats\n"[..], true)] {
        let stream = [prefix, LINEAR].concat();
        let Some((stock, zvcs)) = both("option-quiet", "", &["fast-import"], &stream) else { return };
        same("option-quiet", &stock, &zvcs);
        assert_eq!(stock.stderr.starts_with("fast-import statistics:"), expect_block);
        assert_eq!(stock.stderr, zvcs.stderr);
    }
}

#[test]
fn statistics_counters_follow_the_tree_model() {
    // These runs read objects back out of a pack — fast-import's own while it
    // is being written, or the kept one when the refs are written and their
    // objects checked — so the four window counters are set aside; every other
    // line must match.
    let branchy = [BRANCHY, DONE].concat();
    for (args, stream) in [
        (&["fast-import"][..], &branchy[..]),
        (&["fast-import", "--active-branches=1"], &branchy),
        (&["fast-import", "--active-branches=0"], &branchy),
        (&["fast-import", "--depth=1"], &branchy),
        (&["fast-import", "--big-file-threshold=10"], &branchy),
        (&["-c", "pack.depth=0", "fast-import"], &branchy),
        (&["-c", "fastimport.unpackLimit=0", "fast-import"], &branchy),
        (&["-c", "fastimport.unpackLimit=0", "fast-import"], LINEAR),
    ] {
        let Some((stock, zvcs)) = both("branchy", "", args, stream) else { return };
        same("branchy", &stock, &zvcs);
        assert!(stock.stderr.contains("fast-import statistics:"), "{args:?}: {}", stock.stderr);
        assert_eq!(
            without_window_counters(&stock.stderr),
            without_window_counters(&zvcs.stderr),
            "{args:?}: the statistics block"
        );
    }
}

#[test]
fn statistics_memory_and_marks_past_the_first_block() {
    // 1500 paths in 37 directories, a mark above 2^20, and 300 notes, whose
    // count moves the notes tree to a fanout of 1: the tree-entry pool grows
    // past its first 1000 entries and the mark table gains two levels.
    let mut stream = b"blob\nmark :1\ndata 3\nab\n\ncommit refs/heads/main\nmark :2\ncommitter A <a@x> 1700000000 +0000\ndata 2\nm\n".to_vec();
    for i in 1..=1500 {
        stream.extend_from_slice(format!("M 100644 :1 d{}/f{i}\n", i % 37).as_bytes());
    }
    stream.push(b'\n');
    for c in 1..=300u32 {
        let body = format!("content number {c}");
        stream.extend_from_slice(
            format!(
                "commit refs/heads/b{}\nmark :{}\ncommitter A <a@x> {} +0000\ndata 1\nx\nfrom :2\nM 100644 inline f\ndata {}\n{body}\n\n",
                c % 9,
                3_000_000 + c,
                1_700_000_000 + c,
                body.len()
            )
            .as_bytes(),
        );
    }
    stream.extend_from_slice(b"commit refs/notes/commits\ncommitter A <a@x> 1700001000 +0000\ndata 1\nn\n");
    for c in 1..=300u32 {
        stream.extend_from_slice(format!("N :1 :{}\n", 3_000_000 + c).as_bytes());
    }
    stream.push(b'\n');

    let Some((stock, zvcs)) = both("big", "", &["fast-import"], &stream) else { return };
    same("big", &stock, &zvcs);
    assert!(stock.stderr.contains("      marks:     1073741824 (       302 unique    )"), "{}", stock.stderr);
    assert_eq!(without_window_counters(&stock.stderr), without_window_counters(&zvcs.stderr));
}

#[test]
fn crash_report_matches_stock() {
    // Each stream dies part way: on an undeclared mark inside a commit whose
    // branch is loaded and dirty, on an unknown command after a checkpoint, and
    // on `--done` with no `done`.
    let undeclared = [LINEAR, b"commit refs/heads/other\ncommitter A <a@x> 1700000003 +0000\ndata 4\nfour\nfrom :2\nM 100644 :1 b.txt\nM 100644 :9 c.txt\n\n"].concat();
    let unknown = [BRANCHY, b"bogus-command\n"].concat();
    for (tag, subdir, args, stream) in [
        ("undeclared", "", &["fast-import"][..], &undeclared[..]),
        ("undeclared-export", "", &["fast-import", "--export-marks=m.out"][..], &undeclared[..]),
        ("undeclared-subdir", "sub", &["fast-import", "--quiet", "--export-marks=m.out"][..], &undeclared[..]),
        ("relative", "", &["fast-import", "--relative-marks", "--export-marks=r.out"][..], &undeclared[..]),
        ("unknown", "", &["fast-import"][..], &unknown[..]),
        ("unknown-lru", "", &["fast-import", "--active-branches=1"][..], &unknown[..]),
        ("done", "", &["fast-import", "--done"][..], LINEAR),
        ("import-marks", "", &["fast-import", "--import-marks=nosuch"][..], LINEAR),
    ] {
        let Some((stock, zvcs)) = both(tag, subdir, args, stream) else { return };
        same(tag, &stock, &zvcs);
        assert_eq!(stock.code, 128, "{tag}: {}", stock.stderr);
        assert!(stock.stderr.contains("fast-import: dumping crash report to .git/fast_import_crash_"), "{tag}: {}", stock.stderr);
        assert_eq!(normalise(&stock.stderr), normalise(&zvcs.stderr), "{tag}: stderr");
        let (Some(s), Some(z)) = (&stock.report, &zvcs.report) else {
            panic!("{tag}: a report on each side (stock {}, zvcs {})", stock.report.is_some(), zvcs.report.is_some());
        };
        assert_eq!(normalise(s), normalise(z), "{tag}: the crash report");
    }
}

#[test]
fn crash_report_keeps_the_last_hundred_commands() {
    let mut stream = Vec::new();
    for i in 0..120 {
        stream.extend_from_slice(format!("blob\nmark :{}\ndata 3\n{:02}\n\n", i + 1, i % 100).as_bytes());
    }
    stream.extend_from_slice(b"# a comment is a command too\nbogus\n");
    let Some((stock, zvcs)) = both("history", "", &["fast-import"], &stream) else { return };
    same("history", &stock, &zvcs);
    let (Some(s), Some(z)) = (&stock.report, &zvcs.report) else { panic!("no report") };
    assert!(s.contains("\n* bogus\n"), "{s}");
    assert_eq!(normalise(s), normalise(z));
}

#[test]
fn a_usage_error_writes_no_crash_report() {
    let Some((stock, zvcs)) = both("usage", "", &["fast-import", "--no-such-flag"], LINEAR) else { return };
    same("usage", &stock, &zvcs);
    assert_eq!(stock.code, 129);
    assert!(stock.report.is_none() && zvcs.report.is_none());
    assert_eq!(stock.stderr, zvcs.stderr);
}
