//! The `git diff` / `git diff --no-index` / `git diff-tree` behaviour that used to be
//! refused, silently dropped, or rendered wrong.
//!
//! Every expectation below was measured against stock git 2.55.0 first and is written
//! out verbatim; nothing here is derived from what this port happens to produce.
//!
//! Four groups:
//!
//! * a tracked path whose name a *directory* has taken — a plain one (an ordinary
//!   deletion) and a checked-out repository (a `100644 => 160000` type change, whose
//!   diffstat counts the blob's lines against the gitlink's one-line image);
//! * the `xdiff` knobs `git diff` used to accept-and-ignore or refuse outright —
//!   `-I<re>`, `--ignore-blank-lines`, `--inter-hunk-context=<n>`, `-a`;
//! * the queue and output-stream options — `-D`, `--skip-to`/`--rotate-to`,
//!   `--output=<file>`, and `--binary`/`-s`'s effect on the output format;
//! * the same set reaching `--no-index`, plus `diff-tree`'s block separator and its
//!   non-`-z` `--numstat` rename record.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn ok(dir: &Path, args: &[&str]) {
    let (o, e, code) = run(dir, args);
    assert_eq!(code, 0, "git {args:?} failed: {o}{e}");
}

fn stdout(dir: &Path, args: &[&str]) -> String {
    let (o, e, code) = run(dir, args);
    assert_eq!(code, 0, "git {args:?} exited {code}: {o}{e}");
    o
}

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-diffirt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    ok(dir, &["init", "-q", "-b", "main"]);
    ok(dir, &["config", "user.email", "t@e.x"]);
    ok(dir, &["config", "user.name", "t"]);
}

fn write(dir: &Path, name: &str, body: &[u8]) {
    std::fs::write(dir.join(name), body).unwrap();
}

// ---------------------------------------------------------------------------
// a tracked path a directory has taken
// ---------------------------------------------------------------------------

/// `check_removed()` (diff-lib.c:22) reports a tracked blob whose name a *plain*
/// directory now holds as simply removed, so every format renders an ordinary
/// deletion at exit 0. This used to die with "…is deleted from the index but a
/// directory now has that name…" for the patch and the diffstat alike.
#[test]
fn blob_replaced_by_plain_directory_renders_a_deletion() {
    let repo = scratch("plain-dir");
    init(&repo);
    write(&repo, "f", b"one\ntwo\nthree\n");
    write(&repo, "g", b"k\n");
    ok(&repo, &["add", "f", "g"]);
    ok(&repo, &["commit", "-q", "-m", "c0"]);
    std::fs::remove_file(repo.join("f")).unwrap();
    std::fs::create_dir(repo.join("f")).unwrap();
    write(&repo, "f/inner", b"inner\n");

    assert_eq!(
        stdout(&repo, &["diff"]),
        "diff --git a/f b/f\n\
         deleted file mode 100644\n\
         index 4cb29ea..0000000\n\
         --- a/f\n\
         +++ /dev/null\n\
         @@ -1,3 +0,0 @@\n\
         -one\n\
         -two\n\
         -three\n"
    );
    assert_eq!(stdout(&repo, &["diff", "--stat"]), " f | 3 ---\n 1 file changed, 3 deletions(-)\n");
    // `-R` moves the worktree root onto the pre-image, so the same collision has to
    // be answered on the other side of the pair.
    assert_eq!(
        stdout(&repo, &["diff", "-R"]),
        "diff --git b/f a/f\n\
         new file mode 100644\n\
         index 0000000..4cb29ea\n\
         --- /dev/null\n\
         +++ a/f\n\
         @@ -0,0 +1,3 @@\n\
         +one\n\
         +two\n\
         +three\n"
    );
    // `git diff HEAD` walks the tree instead of the index and must agree.
    assert_eq!(stdout(&repo, &["diff", "HEAD", "--stat"]), " f | 3 ---\n 1 file changed, 3 deletions(-)\n");
}

/// The other half of `check_removed()`: when the directory *is* a repository,
/// `resolve_gitlink_ref()` succeeds, the pair keeps `ce_mode_from_stat()`'s
/// `S_IFGITLINK`, and the change is a `100644 => 160000` type change. The diffstat
/// sees the pair whole — `builtin_diffstat()` counts the blob's three lines against
/// the one-line image `diff_populate_filespec()` synthesises for the gitlink — while
/// the patch is split into a deletion and a creation by `run_diff()`.
#[test]
fn blob_replaced_by_checked_out_submodule_is_a_type_change() {
    let repo = scratch("gitlink-dir");
    init(&repo);
    write(&repo, "f", b"one\ntwo\nthree\n");
    write(&repo, "g", b"k\n");
    ok(&repo, &["add", "f", "g"]);
    ok(&repo, &["commit", "-q", "-m", "c0"]);
    std::fs::remove_file(repo.join("f")).unwrap();
    let sub = repo.join("f");
    init(&sub);
    write(&sub, "s", b"s\n");
    ok(&sub, &["add", "s"]);
    ok(&sub, &["commit", "-q", "-m", "s0"]);
    let head = stdout(&sub, &["rev-parse", "HEAD"]).trim().to_owned();

    assert_eq!(stdout(&repo, &["diff", "--raw"]), ":100644 160000 4cb29ea 0000000 T\tf\n");
    assert_eq!(stdout(&repo, &["diff", "--name-status"]), "T\tf\n");
    assert_eq!(stdout(&repo, &["diff", "--summary"]), " mode change 100644 => 160000 f\n");
    // 1 insertion for `Subproject commit <oid>`, 3 deletions for the blob it replaced.
    assert_eq!(stdout(&repo, &["diff", "--numstat"]), "1\t3\tf\n");
    assert_eq!(stdout(&repo, &["diff", "--stat"]), " f | 4 +---\n 1 file changed, 1 insertion(+), 3 deletions(-)\n");
    assert_eq!(
        stdout(&repo, &["diff", "-p"]),
        format!(
            "diff --git a/f b/f\n\
             deleted file mode 100644\n\
             index 4cb29ea..0000000\n\
             --- a/f\n\
             +++ /dev/null\n\
             @@ -1,3 +0,0 @@\n\
             -one\n\
             -two\n\
             -three\n\
             diff --git a/f b/f\n\
             new file mode 160000\n\
             index 0000000..{short}\n\
             --- /dev/null\n\
             +++ b/f\n\
             @@ -0,0 +1 @@\n\
             +Subproject commit {head}\n",
            short = &head[..7],
        )
    );
}

// ---------------------------------------------------------------------------
// the xdiff knobs
// ---------------------------------------------------------------------------

/// A repository whose worktree exercises `-I`, `--ignore-blank-lines`,
/// `--inter-hunk-context` and `-a` at once.
fn xdiff_fixture(tag: &str) -> PathBuf {
    let repo = scratch(tag);
    init(&repo);
    write(&repo, "log.txt", b"h\nTS 1\nb1\nb2\nTS 2\nz\n");
    write(&repo, "blank.txt", b"p\n\nq\n");
    write(&repo, "far.txt", b"l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\nl12\n");
    write(&repo, "bin.dat", b"A\0B\nC\n");
    ok(&repo, &["add", "."]);
    ok(&repo, &["commit", "-q", "-m", "c0"]);
    write(&repo, "log.txt", b"h\nTS 9\nb1\nb2\nTS 8\nz\n");
    write(&repo, "blank.txt", b"p\n\n\n\nq\n");
    write(&repo, "far.txt", b"l1X\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\nl12X\n");
    write(&repo, "bin.dat", b"A\0B\nD\n");
    repo
}

/// `-I<re>` marks a change whose every record matches as ignorable
/// (`xdl_mark_ignorable_regex`), so a file with nothing else to show drops out of the
/// patch *and* the counts. It also raises `diff_from_contents` (diff.c:4899), which is
/// what makes the raw and name formats drop the pair too — and, as a side effect of
/// rendering each pair through `run_diff()` first, what leaves
/// `diff_fill_oid_info()`'s real hash on the worktree side of the `--raw` record.
#[test]
fn ignore_matching_lines_drops_the_pair_from_every_format() {
    let repo = xdiff_fixture("ignore-lines");

    // log.txt is gone from all three; the surviving worktree ids are real, not zeros.
    assert_eq!(
        stdout(&repo, &["diff", "-I", "^TS", "--numstat"]),
        "-\t-\tbin.dat\n2\t0\tblank.txt\n2\t2\tfar.txt\n"
    );
    assert_eq!(
        stdout(&repo, &["diff", "-I", "^TS", "--raw"]),
        ":100644 100644 6c1d401 e68fd7d M\tbin.dat\n\
         :100644 100644 19a69b7 7c7a15e M\tblank.txt\n\
         :100644 100644 1b2a1c5 06d3493 M\tfar.txt\n"
    );
    assert_eq!(
        stdout(&repo, &["diff", "-I", "^TS", "--name-status"]),
        "M\tbin.dat\nM\tblank.txt\nM\tfar.txt\n"
    );
    // Glued, separated and long spellings all reach the same `regcomp`.
    let glued = stdout(&repo, &["diff", "-I^TS", "--name-status"]);
    assert_eq!(glued, stdout(&repo, &["diff", "--ignore-matching-lines=^TS", "--name-status"]));
    assert_eq!(glued, stdout(&repo, &["diff", "--ignore-matching-lines", "^TS", "--name-status"]));

    // `diff_opt_ignore_regex`'s own error, at 129, before anything is rendered.
    let (_, err, code) = run(&repo, &["diff", "-I", "[", "--name-status"]);
    assert_eq!((err.as_str(), code), ("error: invalid regex given to -I: '['\n", 129));
    // A bare `-I` at the end of the line never reaches its callback.
    let (_, err, code) = run(&repo, &["diff", "-I"]);
    assert_eq!((err.as_str(), code), ("error: switch `I' requires a value\n", 129));
}

/// `--ignore-blank-lines` is `XDF_IGNORE_BLANK_LINES`, which
/// `xdl_mark_ignorable_lines` turns into the same `ignore` bit — but it is *not* on
/// `diff_setup_done()`'s `diff_from_contents` list, and the difference shows: the pair
/// leaves the counts yet keeps its `--raw` record, still with the all-zero post-image
/// name a worktree side normally has.
#[test]
fn ignore_blank_lines_drops_the_counts_but_not_the_raw_record() {
    let repo = xdiff_fixture("blank-lines");

    assert_eq!(
        stdout(&repo, &["diff", "--ignore-blank-lines", "--numstat"]),
        "-\t-\tbin.dat\n2\t2\tfar.txt\n2\t2\tlog.txt\n"
    );
    assert_eq!(
        stdout(&repo, &["diff", "--ignore-blank-lines", "--raw"]),
        ":100644 100644 6c1d401 0000000 M\tbin.dat\n\
         :100644 100644 19a69b7 0000000 M\tblank.txt\n\
         :100644 100644 1b2a1c5 0000000 M\tfar.txt\n\
         :100644 100644 7429912 0000000 M\tlog.txt\n"
    );
    assert!(
        !stdout(&repo, &["diff", "--ignore-blank-lines", "--", "blank.txt"]).contains("@@"),
        "a blank-only change must render no hunk"
    );
}

/// `--inter-hunk-context=<n>` is `xecfg.interhunkctxlen`: two changes that far apart
/// share one hunk instead of getting one each.
#[test]
fn inter_hunk_context_merges_neighbouring_hunks() {
    let repo = xdiff_fixture("inter-hunk");

    let split = stdout(&repo, &["diff", "-U1", "--", "far.txt"]);
    assert!(split.contains("@@ -1,2 +1,2 @@\n"), "{split}");
    assert!(split.contains("@@ -11,2 +11,2 @@ l10\n"), "{split}");

    let merged = stdout(&repo, &["diff", "-U1", "--inter-hunk-context=8", "--", "far.txt"]);
    assert!(merged.contains("@@ -1,12 +1,12 @@\n"), "{merged}");
    assert_eq!(merged.matches("@@ -").count(), 1, "the two changes now share one hunk: {merged}");
    // The separated spelling reaches the same `OPT_UNSIGNED`.
    assert_eq!(merged, stdout(&repo, &["diff", "-U1", "--inter-hunk-context", "8", "--", "far.txt"]));

    let (_, err, code) = run(&repo, &["diff", "--inter-hunk-context="]);
    assert_eq!(
        (err.as_str(), code),
        ("error: option `inter-hunk-context' expects a numerical value\n", 129)
    );
    let (_, err, code) = run(&repo, &["diff", "--inter-hunk-context=bad"]);
    assert_eq!(code, 129, "{err}");
    assert!(err.contains("non-negative integer value"), "{err}");
}

/// `-a`/`--text` drops out of `builtin_diff()`'s binary test, so the patch shows
/// hunks — while `builtin_diffstat()`, which never reads the flag, keeps reporting the
/// file as binary.
#[test]
fn text_forces_a_patch_but_not_the_diffstat() {
    let repo = xdiff_fixture("text");

    let plain = stdout(&repo, &["diff", "--", "bin.dat"]);
    assert!(plain.contains("Binary files a/bin.dat and b/bin.dat differ\n"), "{plain}");

    let forced = stdout(&repo, &["diff", "-a", "--", "bin.dat"]);
    assert!(!forced.contains("Binary files"), "{forced}");
    assert!(forced.contains("@@ -1,2 +1,2 @@\n"), "{forced}");
    assert!(forced.contains("-C\n+D\n"), "{forced}");
    assert_eq!(forced, stdout(&repo, &["diff", "--text", "--", "bin.dat"]));
    // `--binary`'s payload lives inside the arm `-a` skips, so it never appears.
    assert!(!stdout(&repo, &["diff", "-a", "--binary", "--", "bin.dat"]).contains("GIT binary patch"));

    assert_eq!(stdout(&repo, &["diff", "-a", "--numstat", "--", "bin.dat"]), "-\t-\tbin.dat\n");
    assert_eq!(stdout(&repo, &["diff", "--text", "--no-text", "--", "bin.dat"]), plain);
}

// ---------------------------------------------------------------------------
// the queue and the output stream
// ---------------------------------------------------------------------------

fn queue_fixture(tag: &str) -> PathBuf {
    let repo = scratch(tag);
    init(&repo);
    write(&repo, "aaa", b"a1\na2\n");
    write(&repo, "bbb", b"b1\n");
    write(&repo, "ccc", b"c1\nc2\n");
    ok(&repo, &["add", "."]);
    ok(&repo, &["commit", "-q", "-m", "c0"]);
    write(&repo, "aaa", b"a1\na2X\n");
    std::fs::remove_file(repo.join("bbb")).unwrap();
    write(&repo, "ccc", b"c1\nc2X\n");
    repo
}

/// `-D`/`--irreversible-delete`: `builtin_diff()` (diff.c:3596) emits the header of a
/// pair whose post-image label is `/dev/null` and jumps to the end, so the deletion
/// keeps its `deleted file mode` and `index` lines and loses everything below them.
/// No other format reads the flag.
#[test]
fn irreversible_delete_stops_a_deletion_at_its_header() {
    let repo = queue_fixture("irreversible");

    assert_eq!(
        stdout(&repo, &["diff", "-D"]),
        "diff --git a/aaa b/aaa\n\
         index 0016606..bc7d630 100644\n\
         --- a/aaa\n\
         +++ b/aaa\n\
         @@ -1,2 +1,2 @@\n\
         \x20a1\n\
         -a2\n\
         +a2X\n\
         diff --git a/bbb b/bbb\n\
         deleted file mode 100644\n\
         index c9c6af7..0000000\n\
         diff --git a/ccc b/ccc\n\
         index d0aaf97..ac858ec 100644\n\
         --- a/ccc\n\
         +++ b/ccc\n\
         @@ -1,2 +1,2 @@\n\
         \x20c1\n\
         -c2\n\
         +c2X\n"
    );
    assert_eq!(stdout(&repo, &["diff", "--irreversible-delete"]), stdout(&repo, &["diff", "-D"]));
    // The stat formats are untouched: the deletion still contributes its line.
    assert_eq!(stdout(&repo, &["diff", "-D", "--numstat"]), "1\t1\taaa\n0\t1\tbbb\n1\t1\tccc\n");
}

/// `diffcore_rotate()` (diff.c:6763) re-anchors the queue on the named pair —
/// `--skip-to` drops what came before it, `--rotate-to` wraps it to the end — and
/// `cmd_diff()`'s `rotate_to_strict` makes a target that names no pair fatal.
#[test]
fn skip_to_and_rotate_to_reanchor_the_queue() {
    let repo = queue_fixture("rotate");

    assert_eq!(stdout(&repo, &["diff", "--skip-to=ccc", "--name-only"]), "ccc\n");
    assert_eq!(stdout(&repo, &["diff", "--rotate-to=ccc", "--name-only"]), "ccc\naaa\nbbb\n");
    // Both spell their value either way.
    assert_eq!(
        stdout(&repo, &["diff", "--skip-to", "ccc", "--name-only"]),
        stdout(&repo, &["diff", "--skip-to=ccc", "--name-only"])
    );
    // The last one on the line is the one `diffcore_std()` reads.
    assert_eq!(
        stdout(&repo, &["diff", "--skip-to=aaa", "--rotate-to=ccc", "--name-only"]),
        "ccc\naaa\nbbb\n"
    );

    let (_, err, code) = run(&repo, &["diff", "--skip-to=zzz"]);
    assert_eq!((err.as_str(), code), ("fatal: No such path 'zzz' in the diff\n", 128));

    // `diffcore_rotate()` opens with `if (!q->nr) return;`, so an empty queue accepts
    // any target rather than dying.
    let clean = scratch("rotate-clean");
    init(&clean);
    write(&clean, "only", b"x\n");
    ok(&clean, &["add", "only"]);
    ok(&clean, &["commit", "-q", "-m", "c0"]);
    assert_eq!(run(&clean, &["diff", "--skip-to=nowhere"]), (String::new(), String::new(), 0));
}

/// `diff_opt_output`'s `xfopen(arg, "w")` runs during the option scan, so the file is
/// created and truncated there and every rendered byte goes to it instead of stdout.
#[test]
fn output_file_takes_the_whole_diff_stream() {
    let repo = queue_fixture("output");

    let (out, err, code) = run(&repo, &["diff", "--output=OUT", "--stat"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "", 0));
    assert_eq!(
        std::fs::read_to_string(repo.join("OUT")).unwrap(),
        " aaa | 2 +-\n bbb | 1 -\n ccc | 2 +-\n 3 files changed, 2 insertions(+), 3 deletions(-)\n"
    );
    // The separated spelling, and the exit status still reflects the diff.
    let (out, _, code) = run(&repo, &["diff", "--output", "OUT2", "--exit-code"]);
    assert_eq!((out.as_str(), code), ("", 1));
    assert!(std::fs::read_to_string(repo.join("OUT2")).unwrap().starts_with("diff --git a/aaa b/aaa\n"));

    // The failure is `xfopen`'s `die()`, at 128, ahead of anything the diff would say.
    let (_, err, code) = run(&repo, &["diff", "--output=nodir/OUT"]);
    assert_eq!(code, 128, "{err}");
    assert!(err.starts_with("fatal: could not open 'nodir/OUT' for writing: "), "{err}");
}

/// Two `diff_setup_done()` rules about the output-format bitmask: `diff_opt_binary()`
/// calls `enable_patch_output()` before setting its own flag, and `-s` is an
/// *assignment* that wipes whatever came before it while leaving whatever comes after.
#[test]
fn binary_enables_the_patch_and_no_patch_assigns_the_format() {
    let repo = xdiff_fixture("format-bits");

    let both = stdout(&repo, &["diff", "--binary", "--stat", "--", "bin.dat"]);
    assert!(both.starts_with(" bin.dat | Bin 6 -> 6 bytes\n"), "{both}");
    assert!(both.contains("\n\nGIT binary patch\n") || both.contains("GIT binary patch\n"), "{both}");

    // `-s` first: the later `--stat` survives it.
    let late = stdout(&repo, &["diff", "-s", "--stat"]);
    assert!(late.ends_with("4 files changed, 6 insertions(+), 4 deletions(-)\n"), "{late}");
    // `-s` last: it wipes the format that came before.
    assert_eq!(stdout(&repo, &["diff", "--stat", "-s"]), "");
    // …including `--name-only`, which is why this is *not* the mutual-exclusion fatal.
    assert_eq!(run(&repo, &["diff", "--name-only", "-s"]), (String::new(), String::new(), 0));
    // The other order keeps both bits and is.
    let (_, err, code) = run(&repo, &["diff", "-s", "--name-only"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(
        err,
        "fatal: options '--name-only', '--name-status', '--check', and '-s' cannot be used together\n"
    );
}

// ---------------------------------------------------------------------------
// --no-index
// ---------------------------------------------------------------------------

fn no_index_fixture(tag: &str) -> PathBuf {
    let root = scratch(tag);
    std::fs::create_dir_all(root.join("L")).unwrap();
    std::fs::create_dir_all(root.join("R")).unwrap();
    write(&root, "L/x", b"a\n");
    write(&root, "R/x", b"b\n");
    write(&root, "L/y", b"gone\n");
    write(&root, "L/bin", b"A\0B\n");
    write(&root, "R/bin", b"A\0C\n");
    root
}

/// `add_diff_options()` (diff-no-index.c:372) hands the no-index parser the *whole*
/// `diff_opts` table, so `-D`, `--binary` and `--inter-hunk-context` all belong to it.
/// `--skip-to`/`--rotate-to` do too, but `builtin_diff_no_index()` never raises
/// `rotate_to_strict`, so a target naming no pair is quietly ignored here.
#[test]
fn no_index_takes_the_shared_option_table() {
    let root = no_index_fixture("no-index");
    let d = |args: &[&str]| -> (String, String, i32) { run(&root, args) };

    let (out, _, code) = d(&["diff", "--no-index", "-D", "L", "R"]);
    assert_eq!(code, 1);
    assert!(
        out.ends_with("diff --git a/L/y b/L/y\ndeleted file mode 100644\nindex 286c5f5..0000000\n"),
        "the deletion stops at its header: {out}"
    );

    // `builtin_diffstat()` reads a binary pair's two *sizes*, not its lines.
    let (out, _, _) = d(&["diff", "--no-index", "--stat", "L", "R"]);
    assert_eq!(
        out,
        " {L => R}/bin     | Bin 4 -> 4 bytes\n\
         \x20{L => R}/x       |   2 +-\n\
         \x20L/y => /dev/null |   1 -\n\
         \x203 files changed, 1 insertion(+), 2 deletions(-)\n"
    );

    // `--binary` widens the `index` line of the binary pair and emits the payload.
    let (out, _, _) = d(&["diff", "--no-index", "--binary", "L/bin", "R/bin"]);
    assert!(out.contains("index 2787717b02871c072914baf33306945f9e5cc3c4..b453eb935a8825162d460317fb016233205e692e 100644\n"), "{out}");
    assert!(out.contains("GIT binary patch\nliteral 4\n"), "{out}");
    assert!(!out.contains("Binary files"), "{out}");

    // A missing rotate target is silent, unlike the tracked path's fatal.
    let (out, err, code) = d(&["diff", "--no-index", "--skip-to=zzz", "--name-only", "L", "R"]);
    assert_eq!((err.as_str(), code), ("", 1));
    assert_eq!(out, "R/bin\nR/x\n/dev/null\n");
    // A matching one anchors on the pair's post-image name.
    let (out, _, _) = d(&["diff", "--no-index", "--skip-to=R/x", "--name-only", "L", "R"]);
    assert_eq!(out, "R/x\n/dev/null\n");
}

/// `diff_setup_done()`'s two format rules reach `--no-index` as well: `--name-only`
/// clears every other format bit, and a non-patch format that survives is written
/// before the patch with `DIFF_SYMBOL_SEPARATOR`'s blank line between them.
#[test]
fn no_index_orders_the_format_blocks() {
    let root = no_index_fixture("no-index-fmt");

    let (out, _, _) = run(&root, &["diff", "--no-index", "--name-only", "-p", "L", "R"]);
    assert_eq!(out, "R/bin\nR/x\n/dev/null\n");

    let (out, _, _) = run(&root, &["diff", "--no-index", "--stat", "-p", "L", "R"]);
    let (stat, patch) = out.split_once("\n\ndiff --git ").expect("stat block, blank line, patch");
    assert!(stat.ends_with(" 3 files changed, 1 insertion(+), 2 deletions(-)"), "{stat}");
    assert!(patch.starts_with("a/L/bin b/R/bin\n"), "{patch}");
}

// ---------------------------------------------------------------------------
// diff-tree's shared renderer
// ---------------------------------------------------------------------------

/// `git diff-tree` runs the `diff-tree -z -r --raw | diff-pairs` pipeline in-process,
/// and the internal `-z` must not reach the parts of the renderer that read
/// `o->line_termination`: `DIFF_SYMBOL_SEPARATOR` between two output blocks, and
/// `show_numstat()`'s choice between the `pprint_rename`d name and the NUL-separated
/// field pair.
#[test]
fn diff_tree_does_not_leak_its_internal_nul_terminator() {
    let repo = scratch("diff-tree-z");
    init(&repo);
    write(&repo, "old", b"aaa\nbbb\nccc\n");
    write(&repo, "keep", b"z\n");
    ok(&repo, &["add", "."]);
    ok(&repo, &["commit", "-q", "-m", "c0"]);
    ok(&repo, &["mv", "old", "renamed"]);
    ok(&repo, &["commit", "-q", "-m", "c1"]);

    let out = stdout(&repo, &["diff-tree", "-p", "--stat", "-r", "HEAD~1", "HEAD"]);
    assert!(
        out.contains("2 files changed, 3 insertions(+), 3 deletions(-)\n\ndiff --git a/old b/old\n"),
        "the separator is a newline, not a NUL: {out:?}"
    );
    assert!(!out.contains('\0'), "{out:?}");

    assert_eq!(stdout(&repo, &["diff-tree", "-M", "--numstat", "-r", "HEAD~1", "HEAD"]), "0\t0\told => renamed\n");
    // Under a real `-z` the NUL layout is still what git prints.
    assert_eq!(
        stdout(&repo, &["diff-tree", "-M", "-z", "--numstat", "-r", "HEAD~1", "HEAD"]),
        "0\t0\t\0old\0renamed\0"
    );
}
