//! Three `git diff` output formats whose shape is easy to get subtly wrong, all
//! pinned against stock git 2.50.1's bytes: `--dirstat`, `-W`, and `--no-index`.
//!
//! Each one has a specific trap:
//!
//!   * `--dirstat`'s default mode weighs a file by *bytes* changed
//!     (`diffcore_count_changes()`), not lines, so a one-line edit in a big file
//!     and a one-line edit in a small one do not weigh the same. Its `lines` mode
//!     does count lines, and charges a binary file in 64-byte units.
//!   * `-W` grows both ends of every hunk to the enclosing function, which merges
//!     hunks that would otherwise be separate — a geometry a fixed context size
//!     cannot express.
//!   * `--no-index` compares two names that need not match, which is what the
//!     `a/<lhs> b/<rhs>` header and the `{a => b}/c` stat name exist for, and it
//!     runs with no repository at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .expect("run binary")
}

fn out(dir: &Path, home: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&run(dir, home, args).stdout).into_owned()
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let o = run(dir, home, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-difffmt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    (root, home)
}

/// `--dirstat` charges each directory the share of the change that landed in it,
/// and the default mode measures that in bytes: a directory holding a large
/// rewrite outweighs one holding a single added line even when the line counts
/// say otherwise. `--dirstat=lines` is the mode that counts lines instead, and the
/// two disagreeing is the whole point of having both.
#[test]
fn dirstat_weighs_bytes_by_default_and_lines_on_request() {
    let (root, home) = scratch("dirstat");
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("big")).unwrap();
    std::fs::create_dir_all(repo.join("small")).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "t@e.x"]);
    git(&repo, &home, &["config", "user.name", "t"]);
    // One long line in `big/`, one short line in `small/`.
    std::fs::write(repo.join("big/f"), format!("{}\n", "x".repeat(400))).unwrap();
    std::fs::write(repo.join("small/f"), "a\n").unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-q", "-m", "base"]);
    // Each side gains exactly one line, so `lines` calls them equal while the
    // byte-weighted default puts almost everything in `big/`.
    std::fs::write(repo.join("big/f"), format!("{}\n{}\n", "x".repeat(400), "y".repeat(400)))
        .unwrap();
    std::fs::write(repo.join("small/f"), "a\nb\n").unwrap();

    assert_eq!(
        out(&repo, &home, &["diff", "--dirstat=lines"]),
        "  50.0% big/\n  50.0% small/\n",
        "the lines mode counts one added line on each side"
    );
    assert_eq!(
        out(&repo, &home, &["diff", "--dirstat"]),
        "  99.5% big/\n",
        "the default mode weighs bytes, so small/ falls under the 3% cut-off"
    );
    assert_eq!(
        out(&repo, &home, &["diff", "--dirstat=files"]),
        "  50.0% big/\n  50.0% small/\n",
        "the files mode charges one unit per changed file"
    );
    // A cut-off of 0 lists every directory the default mode would have dropped.
    assert!(
        out(&repo, &home, &["diff", "--dirstat=0"]).contains("small/"),
        "an explicit cut-off of 0 keeps the small directory"
    );

    let bad = run(&repo, &home, &["diff", "--dirstat=nonsense"]);
    assert_eq!(bad.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&bad.stderr),
        "fatal: Failed to parse --dirstat/-X option parameter:\n  Unknown dirstat parameter 'nonsense'\n\n"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `-W` is not "more context": it extends each hunk to the function it sits in,
/// so two edits in one function collapse into a single hunk that starts at the
/// function's own line — which a `-U<n>` large enough to join them would still
/// start somewhere else.
#[test]
fn function_context_grows_hunks_to_whole_functions() {
    let (root, home) = scratch("funcctx");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "t@e.x"]);
    git(&repo, &home, &["config", "user.name", "t"]);
    let base: String = (0..3)
        .map(|f| {
            let body: String = (0..16).map(|i| format!("    int v{f}_{i} = {i};\n")).collect();
            format!("int func{f}(int a)\n{{\n{body}}}\n\n")
        })
        .collect();
    std::fs::write(repo.join("c.c"), &base).unwrap();
    git(&repo, &home, &["add", "c.c"]);
    git(&repo, &home, &["commit", "-q", "-m", "base"]);
    // Two edits inside func1, far enough apart that the default context leaves
    // them in separate hunks.
    let edited = base
        .replace("    int v1_0 = 0;\n", "    int v1_0 = 100;\n")
        .replace("    int v1_15 = 15;\n", "    int v1_15 = 1500;\n");
    std::fs::write(repo.join("c.c"), edited).unwrap();

    // Count hunk *headers*, not occurrences of `@@ ` — a header carries the
    // marker twice once it also names the enclosing function.
    let hunks = |patch: &str| patch.lines().filter(|l| l.starts_with("@@ ")).count();

    let plain = out(&repo, &home, &["diff"]);
    assert_eq!(hunks(&plain), 2, "the default context leaves two hunks:\n{plain}");

    let wide = out(&repo, &home, &["diff", "-W"]);
    assert_eq!(hunks(&wide), 1, "-W merges them into one:\n{wide}");
    // The hunk covers func1 whole: from the blank line above its signature down
    // past its closing brace, which is where the enclosing-function search stops.
    assert!(
        wide.contains("@@ -20,22 +20,22 @@ int func0(int a)\n \n int func1(int a)\n {\n"),
        "the hunk must reach back to the enclosing function:\n{wide}"
    );
    assert!(wide.contains("\n }\n \n int func2(int a)\n"), "and past its close:\n{wide}");
    assert_eq!(
        out(&repo, &home, &["diff", "-W", "--no-function-context"]),
        plain,
        "--no-function-context puts the ordinary geometry back"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `--no-index` runs with no repository, compares two differently-named paths, and
/// exits 1 when they differ (git-diff(1): "this option implies `--exit-code`").
#[test]
fn no_index_compares_two_paths_outside_a_repository() {
    let (root, home) = scratch("noindex");
    std::fs::create_dir_all(root.join("d1")).unwrap();
    std::fs::create_dir_all(root.join("d2")).unwrap();
    std::fs::write(root.join("lhs"), "a\nb\n").unwrap();
    std::fs::write(root.join("rhs"), "a\nc\n").unwrap();
    std::fs::write(root.join("same"), "a\nb\n").unwrap();
    std::fs::write(root.join("d1/p"), "x\n").unwrap();
    std::fs::write(root.join("d2/p"), "y\n").unwrap();
    std::fs::write(root.join("d2/q"), "z\n").unwrap();

    // Nothing here is inside a repository, which is the case that must not need
    // one — discovery has to be skipped before it is attempted.
    assert!(!root.join(".git").exists());

    let differ = run(&root, &home, &["diff", "--no-index", "lhs", "rhs"]);
    assert_eq!(differ.status.code(), Some(1), "a difference exits 1");
    assert_eq!(
        String::from_utf8_lossy(&differ.stdout),
        "diff --git a/lhs b/rhs\n\
         index 422c2b7..0f7bc76 100644\n\
         --- a/lhs\n\
         +++ b/rhs\n\
         @@ -1,2 +1,2 @@\n \
         a\n\
         -b\n\
         +c\n",
        "the two sides keep their own names in the header"
    );

    let same = run(&root, &home, &["diff", "--no-index", "lhs", "same"]);
    assert_eq!(same.status.code(), Some(0), "identical files exit 0");
    assert!(same.stdout.is_empty());

    // `/dev/null` makes the pair an addition, and both halves of the header take
    // the name of the side that exists.
    let added = out(&root, &home, &["diff", "--no-index", "/dev/null", "lhs"]);
    assert!(added.starts_with("diff --git a/lhs b/lhs\nnew file mode 100644\n"), "{added}");
    assert!(added.contains("--- /dev/null\n+++ b/lhs\n"), "{added}");

    // A directory operand walks both sides; a name on one side only is an
    // addition, and the stat name compacts the differing components.
    let dirs = out(&root, &home, &["diff", "--no-index", "--stat", "d1", "d2"]);
    assert_eq!(
        dirs,
        " {d1 => d2}/p      | 2 +-\n \
         /dev/null => d2/q | 1 +\n \
         2 files changed, 2 insertions(+), 1 deletion(-)\n",
        "{dirs}"
    );

    let missing = run(&root, &home, &["diff", "--no-index", "lhs", "nope"]);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&missing.stderr), "error: Could not access 'nope'\n");

    let _ = std::fs::remove_dir_all(&root);
}

/// `--stat` on a binary pair prints the two *sizes*, not a line count — the
/// numbers come out of the filespecs rather than the diff, and printing a bare
/// `Bin` loses them.
#[test]
fn stat_reports_binary_sizes() {
    let (root, home) = scratch("binstat");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "t@e.x"]);
    git(&repo, &home, &["config", "user.name", "t"]);
    std::fs::write(repo.join("b.dat"), [0u8, 1, 2, 3, 4]).unwrap();
    git(&repo, &home, &["add", "b.dat"]);
    git(&repo, &home, &["commit", "-q", "-m", "base"]);
    std::fs::write(repo.join("b.dat"), [0u8, 1, 2, 3, 4, 5, 6]).unwrap();

    let stat = out(&repo, &home, &["diff", "--stat"]);
    assert_eq!(
        stat,
        " b.dat | Bin 5 -> 7 bytes\n 1 file changed, 0 insertions(+), 0 deletions(-)\n",
        "{stat}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
