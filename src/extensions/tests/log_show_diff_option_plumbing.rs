//! `git log` / `git show` accept the same diff options `git diff` does, because
//! `setup_revisions()` hands every token it does not recognise to `diff_opt_parse()`
//! (revision.c:2721) — and each one has to *change the rendered patch*, not merely be
//! swallowed.
//!
//! That second half is the point of this file. A history command can be made to stop
//! printing `unsupported flag "--patience"` by adding one match arm, and every
//! exit-code-only check will then pass while the patch bytes are still plain Myers.
//! Silently mis-rendering a patch is worse than refusing to render it, so each case
//! below pins the flag's output against the *default* output as well as against the
//! bytes stock git 2.55.0 produces: an accepted-but-unplumbed flag makes the
//! `assert_ne!` fail even though the command exited 0.
//!
//! Every expectation here was measured from stock git 2.55.0. Nothing in this file
//! shells out to stock at run time, so it works on a headless CI box with only the
//! zvcs binary present.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The frobnitz/fib pair from git's own `t/lib-diff-alternative.sh`, which is the
/// input git uses to demonstrate that patience and histogram pick a different
/// common subsequence than Myers. Anything simpler makes all four algorithms agree
/// and would let an unplumbed `--patience` pass unnoticed.
const CODE_BEFORE: &str = "\
#include <stdio.h>

// Frobs foo heartily
int frobnitz(int foo)
{
    int i;
    for(i = 0; i < 10; i++)
    {
        printf(\"Your answer is: \");
        printf(\"%d\\n\", foo);
    }
}

int fact(int n)
{
    if(n > 1)
    {
        return fact(n-1) * n;
    }
    return 1;
}

int main(int argc, char **argv)
{
    frobnitz(fact(10));
}
";

const CODE_AFTER: &str = "\
#include <stdio.h>

int fib(int n)
{
    if(n > 2)
    {
        return fib(n-1) + fib(n-2);
    }
    return 1;
}

// Frobs foo heartily
int frobnitz(int foo)
{
    int i;
    for(i = 0; i < 10; i++)
    {
        printf(\"%d\\n\", foo);
    }
}

int main(int argc, char **argv)
{
    frobnitz(fib(10));
}
";

/// Two blocks with a blank line between them, plus a third inserted between them.
/// The added group can slide either way, so `--no-indent-heuristic` moves it —
/// which is the only observable proof that `XDF_INDENT_HEURISTIC` reached xdiff.
const SLIDE_BEFORE: &str = "  {\n    a();\n  }\n\n  {\n    c();\n  }\n";
const SLIDE_AFTER: &str = "  {\n    a();\n  }\n\n  {\n    b();\n  }\n\n  {\n    c();\n  }\n";

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = Command::new(BIN)
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("ZVCS_HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@e.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@e.com")
            .env("GIT_AUTHOR_DATE", "1136214245 +0000")
            .env("GIT_COMMITTER_DATE", "1136214245 +0000")
            .output()
            .expect("spawn zvcs");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    /// Assert the command succeeded, and return its stdout.
    fn ok(&self, args: &[&str]) -> String {
        let (stdout, stderr, code) = self.run(args);
        assert_eq!(code, 0, "`git {args:?}` exited {code}; stderr: {stderr}");
        assert!(
            !stderr.contains("unsupported"),
            "`git {args:?}` refused the flag: {stderr}"
        );
        stdout
    }

    fn commit(&self, msg: &str) {
        self.ok(&["add", "-A"]);
        self.ok(&["commit", "-q", "-m", msg]);
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.repo.join(name), body).unwrap();
    }
}

/// Every test gets its own repository; the tests run concurrently in one process.
fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "zvcs-diffopt-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let f = Fixture { repo, home };
    f.ok(&["init", "-q", "-b", "main"]);
    f.ok(&["config", "user.name", "A"]);
    f.ok(&["config", "user.email", "a@e.com"]);
    f
}

/// The body of a patch: the `---`/`+++`/`@@` block and everything under it, with the
/// commit header and the `index` line dropped so an abbreviation change cannot make
/// the comparison fail for an unrelated reason.
fn body(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .skip_while(|l| !l.starts_with("@@"))
        .filter(|l| !l.starts_with("index "))
        .collect()
}

/// `--minimal`/`--patience`/`--histogram`/`--diff-algorithm=<v>` reach xdiff from
/// `log -p` and from `show`.
///
/// On this input patience and histogram agree with each other and disagree with
/// Myers, which is exactly the property git's own `t4033`/`t4050` rely on. The
/// `assert_ne!` against the default is what fails if the flags are parsed and then
/// dropped on the floor.
#[test]
fn diff_algorithm_flags_reach_xdiff_on_log_and_show() {
    let f = fixture("algo");
    f.write("code.c", CODE_BEFORE);
    f.commit("c1");
    f.write("code.c", CODE_AFTER);
    f.commit("c2");

    let myers = f.ok(&["log", "-p", "-1"]);
    let patience = f.ok(&["log", "-p", "-1", "--patience"]);
    let histogram = f.ok(&["log", "-p", "-1", "--histogram"]);
    let minimal = f.ok(&["log", "-p", "-1", "--minimal"]);

    assert_ne!(
        body(&myers),
        body(&patience),
        "--patience produced the Myers hunks, so the flag never reached xdiff"
    );
    assert_eq!(
        body(&patience),
        body(&histogram),
        "measured from stock: patience and histogram agree on this input"
    );
    // Stock renders `--minimal` identically to plain Myers here; keeping the
    // assertion pins that it is still *parsed* rather than rejected.
    assert_eq!(body(&myers), body(&minimal));

    // The long spelling is the same option (`parse_algorithm_value()`), and it is
    // case-insensitive.
    for spelling in ["--diff-algorithm=patience", "--diff-algorithm=PATIENCE"] {
        assert_eq!(
            body(&f.ok(&["log", "-p", "-1", spelling])),
            body(&patience),
            "{spelling} disagreed with --patience"
        );
    }

    // `cmd_show` runs the same `setup_revisions()`, so it takes the same flags.
    assert_eq!(
        body(&f.ok(&["show", "--patience", "HEAD"])),
        body(&patience),
        "show and log disagreed under --patience"
    );

    // The exact first added line stock emits under patience — the fib block moves to
    // the top, which is the whole reason the algorithms differ here.
    let first_added = body(&patience)
        .into_iter()
        .find(|l| l.starts_with('+'))
        .unwrap()
        .to_string();
    assert_eq!(first_added, "+int fib(int n)");
}

/// An unknown algorithm name is `diff_opt_diff_algorithm()`'s usage error, not a
/// silent fallback to Myers.
#[test]
fn unknown_diff_algorithm_is_a_usage_error() {
    let f = fixture("algobad");
    f.write("code.c", CODE_BEFORE);
    f.commit("c1");

    let (_, stderr, code) = f.run(&["log", "-p", "-1", "--diff-algorithm=nosuch"]);
    assert_eq!(code, 129, "stderr: {stderr}");
    assert!(
        stderr.contains("diff-algorithm accepts"),
        "unexpected message: {stderr}"
    );
}

/// `--no-indent-heuristic` clears `XDF_INDENT_HEURISTIC`, which moves a slidable
/// added group. With the flag merely accepted, both renderings are identical.
#[test]
fn indent_heuristic_flag_moves_a_slidable_hunk() {
    let f = fixture("slide");
    f.write("s.txt", SLIDE_BEFORE);
    f.commit("c1");
    f.write("s.txt", SLIDE_AFTER);
    f.commit("c2");

    let on = f.ok(&["log", "-p", "-1"]);
    let off = f.ok(&["log", "-p", "-1", "--no-indent-heuristic"]);
    assert_ne!(
        body(&on),
        body(&off),
        "--no-indent-heuristic changed nothing, so the flag never reached xdiff"
    );

    // Measured from stock: the heuristic keeps the block whole (`+  {` first), while
    // clearing it slides the group so the added run starts mid-block.
    let first_added = |p: &str| {
        body(p)
            .into_iter()
            .find(|l| l.starts_with('+'))
            .unwrap()
            .to_string()
    };
    assert_eq!(first_added(&on), "+  {");
    assert_eq!(first_added(&off), "+    b();");

    // `--indent-heuristic` is the default, so naming it explicitly is a no-op.
    assert_eq!(body(&f.ok(&["log", "-p", "-1", "--indent-heuristic"])), body(&on));
    // and `show` agrees with `log` under both.
    assert_eq!(
        body(&f.ok(&["show", "--no-indent-heuristic", "HEAD"])),
        body(&off)
    );
}

/// `--inter-hunk-context=<n>` is `xecfg.interhunkctxlen`: two changes closer than
/// `n` context lines collapse into one hunk. Two hunks becoming one is only
/// observable if the value actually reached xdiff.
#[test]
fn inter_hunk_context_merges_adjacent_hunks() {
    let f = fixture("ihc");
    let before: String = (1..=15).map(|i| format!("l{i}\n")).collect();
    let after: String = (1..=15)
        .map(|i| match i {
            2 => "CHANGED2\n".to_string(),
            10 => "CHANGED10\n".to_string(),
            _ => format!("l{i}\n"),
        })
        .collect();
    f.write("f.txt", &before);
    f.commit("c1");
    f.write("f.txt", &after);
    f.commit("c2");

    let hunks = |p: &str| p.lines().filter(|l| l.starts_with("@@")).count();

    // Measured from stock: two separate hunks by default, one once the gap is
    // allowed to be spanned.
    assert_eq!(hunks(&f.ok(&["log", "-p", "-1"])), 2);
    assert_eq!(hunks(&f.ok(&["log", "-p", "-1", "--inter-hunk-context=10"])), 1);
    assert_eq!(hunks(&f.ok(&["show", "--inter-hunk-context=10", "HEAD"])), 1);
}

/// `--ignore-blank-lines` sets `XDF_IGNORE_BLANK_LINES`, so a change that only adds
/// blank lines stops being reported at all — `log` exits 0 with no patch body.
#[test]
fn ignore_blank_lines_drops_a_blank_only_change() {
    let f = fixture("ibl");
    f.write("f.txt", "a\nb\nc\n");
    f.commit("c1");
    f.write("f.txt", "a\n\n\nb\n\nc\n");
    f.commit("c2");

    let plain = f.ok(&["log", "-p", "-1"]);
    assert!(
        plain.contains("@@"),
        "the blank-only change should be a hunk by default"
    );
    let ignored = f.ok(&["log", "-p", "-1", "--ignore-blank-lines"]);
    assert!(
        !ignored.contains("@@"),
        "--ignore-blank-lines left the blank-only hunk in place: {ignored}"
    );
}

/// `--binary` widens a binary pair from `Binary files … differ` to the base85
/// `GIT binary patch` payload. Accepting the flag without threading it through to
/// the image reader leaves the short form, which this catches.
#[test]
fn binary_flag_emits_the_git_binary_patch_payload() {
    let f = fixture("bin");
    std::fs::write(f.repo.join("b.bin"), [0u8, 1, 2, b'B', 0xff, 0xfe]).unwrap();
    f.commit("c1");
    std::fs::write(f.repo.join("b.bin"), [0u8, 1, 2, b'C', 0xff, 0xfe, 0xfd]).unwrap();
    f.commit("c2");

    let plain = f.ok(&["log", "-p", "-1"]);
    assert!(plain.contains("Binary files"), "{plain}");
    assert!(!plain.contains("GIT binary patch"));

    for args in [
        vec!["log", "-p", "-1", "--binary"],
        vec!["show", "--binary", "HEAD"],
    ] {
        let out = f.ok(&args);
        assert!(
            out.contains("GIT binary patch"),
            "{args:?} did not emit the payload: {out}"
        );
        assert!(!out.contains("Binary files"), "{args:?}: {out}");
    }
}

/// `-D`/`--irreversible-delete` makes `builtin_diff()` stop a deletion after its
/// header, so the removed lines never appear.
#[test]
fn irreversible_delete_drops_the_deleted_body() {
    let f = fixture("irrev");
    f.write("del.txt", "keepme\n");
    f.commit("c1");
    std::fs::remove_file(f.repo.join("del.txt")).unwrap();
    f.commit("c2");

    let plain = f.ok(&["log", "-p", "-1"]);
    assert!(plain.contains("-keepme"), "{plain}");

    for args in [vec!["log", "-p", "-1", "-D"], vec!["show", "-D", "HEAD"]] {
        let out = f.ok(&args);
        assert!(
            out.contains("deleted file mode"),
            "{args:?} lost the deletion header: {out}"
        );
        assert!(
            !out.contains("-keepme"),
            "{args:?} still printed the deleted body: {out}"
        );
    }
}

/// `git show -<n>` is not a one-commit display with a decoration: `-<digit>`,
/// `-n<n>` and `--max-count=<n>` each clear `revs->no_walk` alongside setting
/// `max_count` (revision.c:2345-2346, 2366-2368, 2370-2378), so `cmd_show` takes its
/// `if (!rev.no_walk)` branch and walks (builtin/log.c:694-699).
#[test]
fn show_max_count_turns_the_pending_display_into_a_walk() {
    let f = fixture("maxcount");
    for i in 1..=4 {
        f.write("f.txt", &format!("v{i}\n"));
        f.commit(&format!("c{i}"));
    }
    let commits = |out: &str| out.lines().filter(|l| l.starts_with("commit ")).count();

    // A bare `show` stays a one-commit pending display.
    assert_eq!(commits(&f.ok(&["show"])), 1);
    // Every spelling of max-count walks instead.
    assert_eq!(commits(&f.ok(&["show", "-1"])), 1);
    assert_eq!(commits(&f.ok(&["show", "-3"])), 3);
    assert_eq!(commits(&f.ok(&["show", "-n2"])), 2);
    assert_eq!(commits(&f.ok(&["show", "-n", "2"])), 2);
    assert_eq!(commits(&f.ok(&["show", "--max-count=2"])), 2);
    assert_eq!(commits(&f.ok(&["show", "--max-count=0"])), 0);

    // The limit caps the walk rather than the pending list, so naming more commits
    // than the limit still yields `n` records.
    assert_eq!(commits(&f.ok(&["show", "-2", "HEAD", "HEAD~1", "HEAD~2"])), 2);

    // `--reverse` reverses what survived the limit, so the oldest of the newest two
    // comes first — not the two oldest commits.
    let rev = f.ok(&["show", "-2", "--reverse", "--oneline"]);
    let first = rev.lines().next().unwrap();
    assert!(first.contains("c3"), "expected c3 first, got: {first}");
}

/// A non-numeric `-n` value is `parse_count()`'s fatal, not a silently ignored flag.
#[test]
fn show_rejects_a_non_numeric_max_count() {
    let f = fixture("maxcountbad");
    f.write("f.txt", "v\n");
    f.commit("c1");

    let (_, stderr, code) = f.run(&["show", "-nx"]);
    assert_ne!(code, 0, "`show -nx` should not succeed");
    assert!(stderr.contains("not an integer"), "unexpected: {stderr}");
}

/// `--submodule[=<format>]` on the history commands. A bare `--submodule` is
/// `DIFF_SUBMODULE_LOG` (diff.c:6269), `short` is the default gitlink rendering, and
/// an unknown name is a usage error rather than a silent fallback.
///
/// Only the parse-level contract is asserted here: building a real submodule needs a
/// second repository and `protocol.file.allow`, which is not worth the CI time. The
/// byte-level rendering of each format is covered by `diff_submodule_format.rs`,
/// which shares [`super`]-level renderers with this path.
#[test]
fn submodule_format_is_parsed_by_log_and_show() {
    let f = fixture("submodule");
    f.write("f.txt", "a\n");
    f.commit("c1");
    f.write("f.txt", "b\n");
    f.commit("c2");

    // With no gitlink in the tree every format renders the same patch, but the flag
    // must be accepted rather than refused.
    let plain = f.ok(&["log", "-p", "-1"]);
    for form in ["--submodule", "--submodule=short", "--submodule=log", "--submodule=diff"] {
        assert_eq!(body(&f.ok(&["log", "-p", "-1", form])), body(&plain), "{form}");
        assert_eq!(body(&f.ok(&["show", form, "HEAD"])), body(&plain), "{form}");
    }

    for verb in [vec!["log", "-p", "-1"], vec!["show", "HEAD"]] {
        let mut args = verb.clone();
        args.push("--submodule=nosuch");
        let (_, stderr, code) = f.run(&args);
        assert_eq!(code, 129, "{verb:?} stderr: {stderr}");
        assert!(stderr.contains("--submodule"), "{verb:?}: {stderr}");
    }
}

/// `--ignore-cr-at-eol` is a `Whitespace` mode, not one of the `xpp` ignore bits, so
/// it travels a different field than the flags above and needs its own guard.
#[test]
fn ignore_cr_at_eol_suppresses_a_line_ending_only_change() {
    let f = fixture("crlf");
    // `core.autocrlf` must stay off or the worktree bytes are rewritten under us.
    f.ok(&["config", "core.autocrlf", "false"]);
    f.write("f.txt", "a\nb\nc\n");
    f.commit("c1");
    f.write("f.txt", "a\r\nb\r\nc\r\n");
    f.commit("c2");

    assert!(f.ok(&["log", "-p", "-1"]).contains("@@"));
    for args in [
        vec!["log", "-p", "-1", "--ignore-cr-at-eol"],
        vec!["show", "--ignore-cr-at-eol", "HEAD"],
    ] {
        let out = f.ok(&args);
        assert!(
            !out.contains("@@"),
            "{args:?} still reported the CR-only change: {out}"
        );
    }
}

/// `whatchanged` is behind `--i-still-use-this` in 2.55.0, and classifies each option
/// before handing the whole argv to `git log`'s implementation, so a flag `log` now honors has to be recognised here too — a stale
/// accept-list turns a working option back into `unsupported flag`.
#[test]
fn whatchanged_forwards_the_flags_log_now_honors() {
    let f = fixture("whatchanged");
    f.write("code.c", CODE_BEFORE);
    f.commit("c1");
    f.write("code.c", CODE_AFTER);
    f.commit("c2");

    for flag in [
        "--patience",
        "--histogram",
        "--minimal",
        "--diff-algorithm=patience",
        "--indent-heuristic",
        "--no-indent-heuristic",
        "--text",
        "-W",
        "-D",
    ] {
        let (_, stderr, code) = f.run(&["whatchanged", "--i-still-use-this", "-1", flag]);
        assert_eq!(code, 0, "whatchanged {flag} exited {code}: {stderr}");
        assert!(
            !stderr.contains("unsupported"),
            "whatchanged refused {flag}: {stderr}"
        );
    }

    // and the forwarded flag still changes the patch, i.e. it reached `log`'s renderer.
    let plain = f.ok(&["whatchanged", "--i-still-use-this", "-p", "-1"]);
    let patience = f.ok(&["whatchanged", "--i-still-use-this", "-p", "-1", "--patience"]);
    assert_ne!(body(&plain), body(&patience));
}

/// Two flags that share a `PatchOpts` must not overwrite one another: `-U` and an
/// algorithm are independent fields, and a mis-wired struct shows up as one of them
/// silently winning.
#[test]
fn context_width_and_algorithm_compose() {
    let f = fixture("compose");
    f.write("code.c", CODE_BEFORE);
    f.commit("c1");
    f.write("code.c", CODE_AFTER);
    f.commit("c2");

    let ctx = |p: &str| {
        p.lines()
            .skip_while(|l| !l.starts_with("@@"))
            .skip(1)
            .take_while(|l| l.starts_with(' '))
            .count()
    };
    let u1 = f.ok(&["log", "-p", "-1", "-U1", "--patience"]);
    let u5 = f.ok(&["log", "-p", "-1", "-U5", "--patience"]);
    assert!(
        ctx(&u1) < ctx(&u5),
        "-U did not survive alongside --patience: {} vs {}",
        ctx(&u1),
        ctx(&u5)
    );
    // and the algorithm still applies under a non-default context width.
    assert_ne!(
        body(&f.ok(&["log", "-p", "-1", "-U1"])),
        body(&u1),
        "--patience was lost once -U1 was also given"
    );
}

/// The `index` line's abbreviation and `--full-index` must keep working now that
/// `patch_render` reads more of `PatchOpts` than it used to.
#[test]
fn full_index_still_widens_the_index_line() {
    let f = fixture("fullindex");
    f.write("f.txt", "a\n");
    f.commit("c1");
    f.write("f.txt", "b\n");
    f.commit("c2");

    let index_line = |p: &str| {
        p.lines()
            .find(|l| l.starts_with("index "))
            .unwrap()
            .to_string()
    };
    let short = index_line(&f.ok(&["log", "-p", "-1"]));
    let full = index_line(&f.ok(&["log", "-p", "-1", "--full-index"]));
    assert!(full.len() > short.len(), "{short} vs {full}");
    // Combining it with a non-default algorithm must not drop either.
    let both = index_line(&f.ok(&["log", "-p", "-1", "--full-index", "--patience"]));
    assert_eq!(both, full);
}

/// Guard against the fixture helpers rotting: `body()` must actually find a patch.
#[test]
fn body_helper_extracts_a_patch() {
    let f = fixture("selfcheck");
    f.write("f.txt", "a\n");
    f.commit("c1");
    f.write("f.txt", "b\n");
    f.commit("c2");
    let patch = f.ok(&["log", "-p", "-1"]);
    let b = body(&patch);
    assert!(b.first().is_some_and(|l| l.starts_with("@@")), "{b:?}");
    assert!(b.iter().any(|l| *l == "-a"));
    assert!(b.iter().any(|l| *l == "+b"));
}

