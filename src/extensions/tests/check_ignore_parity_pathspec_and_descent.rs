//! `git check-ignore` parity for the three things that happen to a path before
//! it is matched, and for the descent that matches it.
//!
//! * `parse_pathspec()` refuses an empty element for the whole argument vector
//!   before it parses any element (pathspec.c:637-643), then, per element,
//!   prefixes it, checks its magic, and refuses a leading path that goes through
//!   a symlink (`PATHSPEC_SYMLINK_LEADING_PATH`, builtin/check-ignore.c:94 and
//!   pathspec.c:660-663).
//! * `prefix_path_gently()`'s absolute arm goes through
//!   `abspath_part_inside_repo()` (setup.c:50-106), which dereferences symlinks
//!   outside the work tree and dies through `strbuf_realpath()` for a path whose
//!   leading directories do not exist (abspath.c:128-137).
//! * `prep_exclude()` stops descending the moment a leading directory is itself
//!   excluded, and that directory's pattern becomes the answer for the whole
//!   path (dir.c:1680-1681, :1726-1742, :1819-1820) — even when a deeper
//!   `.gitignore`, or a negation of a deeper directory, would have said
//!   otherwise.
//!
//! Every case here is byte-compared against stock git 2.55.0 by hand; the
//! assertions are the measured output.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

/// A work tree with a directory that is excluded, an inner directory that a
/// later negation un-excludes, and a symlink to a directory.
fn fixture(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-ckign-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(repo.join("ign/inner")).unwrap();
    std::fs::create_dir_all(repo.join("plain")).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "ign/\n!ign/inner/\n").unwrap();
    std::fs::write(repo.join("ign/inner/.gitignore"), "inner.txt\n").unwrap();
    std::fs::write(repo.join("ign/inner/inner.txt"), "").unwrap();
    std::fs::write(repo.join("plain/f.txt"), "").unwrap();
    (repo, home)
}

fn run(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_ATTR_SOURCE")
        .output()
        .unwrap()
}

fn out_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn err_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

/// `prep_exclude()` never reaches `ign/inner/.gitignore`: `ign` is excluded, so
/// the walk returns `ign/` as the pattern for everything below it. The negation
/// on the *next* level down is never consulted, because the level above it
/// already ended the descent.
#[test]
fn an_excluded_leading_directory_decides_the_whole_path() {
    let (repo, home) = fixture("descent");

    let out = run(&home, &repo, &["check-ignore", "-v", "ign/inner/inner.txt"]);
    assert_eq!(out_of(&out), ".gitignore:1:ign/\tign/inner/inner.txt\n");
    assert_eq!(code(&out), 0);

    // The directory itself answers the same way, from the same line.
    let out = run(&home, &repo, &["check-ignore", "-v", "ign/inner"]);
    assert_eq!(out_of(&out), ".gitignore:1:ign/\tign/inner\n");

    // A path with no excluded leading directory still reaches its own lookup.
    let out = run(&home, &repo, &["check-ignore", "-v", "-n", "plain/f.txt"]);
    assert_eq!(out_of(&out), "::\tplain/f.txt\n");
    assert_eq!(code(&out), 1);
}

/// `has_symlink_leading_path()` looks at every `/`-terminated prefix and not at
/// the last component, so a symlink *is* reportable in its own right while a
/// path reached *through* one is refused before any matching happens.
#[test]
#[cfg(unix)]
fn a_pathspec_reached_through_a_symlink_is_refused() {
    let (repo, home) = fixture("symlink");
    std::os::unix::fs::symlink("plain", repo.join("slink")).unwrap();

    let out = run(&home, &repo, &["check-ignore", "-v", "-n", "slink/f.txt"]);
    assert_eq!(err_of(&out), "fatal: pathspec 'slink/f.txt' is beyond a symbolic link\n");
    assert_eq!(out_of(&out), "");
    assert_eq!(code(&out), 128);

    // The symlink itself is the last component, which the walk never lstat()s.
    let out = run(&home, &repo, &["check-ignore", "-v", "-n", "slink"]);
    assert_eq!(out_of(&out), "::\tslink\n");
    assert_eq!(code(&out), 1);

    // The refusal happens while the whole argument vector is parsed, so the
    // match that would have been printed for an earlier argument is suppressed.
    let out = run(
        &home,
        &repo,
        &["check-ignore", "-v", "ign/inner/inner.txt", "slink/f.txt"],
    );
    assert_eq!(out_of(&out), "");
    assert_eq!(err_of(&out), "fatal: pathspec 'slink/f.txt' is beyond a symbolic link\n");
    assert_eq!(code(&out), 128);
}

/// `--stdin` calls `parse_pathspec()` once per line and flushes after each one
/// (builtin/check-ignore.c:138-149), so a die on a later line keeps the records
/// the earlier lines produced — the opposite of the whole-vector case above.
#[test]
#[cfg(unix)]
fn stdin_keeps_the_records_written_before_a_mid_stream_die() {
    use std::io::Write;

    let (repo, home) = fixture("stdin");
    std::os::unix::fs::symlink("plain", repo.join("slink")).unwrap();

    let mut child = Command::new(BIN)
        .args(["check-ignore", "-v", "--stdin"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"ign/inner/inner.txt\nslink/f.txt\nign/inner/inner.txt\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();

    assert_eq!(out_of(&out), ".gitignore:1:ign/\tign/inner/inner.txt\n");
    assert_eq!(err_of(&out), "fatal: pathspec 'slink/f.txt' is beyond a symbolic link\n");
    assert_eq!(code(&out), 128);
}

/// The empty-element refusal is raised for the whole vector before any element
/// is parsed, so it outranks the per-element magic and path diagnostics that
/// would otherwise have fired on an earlier argument.
#[test]
fn an_empty_element_outranks_every_per_element_diagnostic() {
    let (repo, home) = fixture("empty");
    const EMPTY: &str = "fatal: empty string is not a valid pathspec. \
                         please use . instead if you meant to match all paths\n";

    for first in ["ign/inner/inner.txt", ":(glob)x", "../outside"] {
        let out = run(&home, &repo, &["check-ignore", "-v", first, ""]);
        assert_eq!(err_of(&out), EMPTY, "first argument {first}");
        assert_eq!(out_of(&out), "", "first argument {first}");
        assert_eq!(code(&out), 128, "first argument {first}");
    }

    // An empty path *after* the magic is not an empty element: `:(top)` alone
    // leaves `item->match` empty, matches nothing and exits 1.
    let out = run(&home, &repo, &["check-ignore", "-v", "-n", ":(top)"]);
    assert_eq!(out_of(&out), "::\t:(top)\n");
    assert_eq!(code(&out), 1);
}

/// `unsupported_magic()` runs after `init_pathspec_item()` has prefixed the
/// path (pathspec.c:653-658), so a path that cannot be resolved is reported even
/// when the element also carries magic this command does not take.
#[test]
fn the_path_is_resolved_before_its_magic_is_rejected() {
    let (repo, home) = fixture("order");

    let out = run(
        &home,
        &repo,
        &["check-ignore", "-v", ":(glob)/zvcs-no-such-root/x"],
    );
    assert_eq!(
        err_of(&out),
        "fatal: Invalid path '/zvcs-no-such-root': No such file or directory\n"
    );
    assert_eq!(code(&out), 128);

    // With a path that resolves, the magic is what is reported.
    let out = run(&home, &repo, &["check-ignore", "-v", ":(glob)plain/f.txt"]);
    assert_eq!(
        err_of(&out),
        "fatal: :(glob)plain/f.txt: pathspec magic not supported by this command: 'glob'\n"
    );
    assert_eq!(code(&out), 128);
}

/// `abspath_part_inside_repo()` compares the *resolved* spelling of each
/// `/`-terminated level against the work tree, so a path that reaches the work
/// tree through a symlink is inside it; a path whose leading directory is not
/// there at all dies through `strbuf_realpath()` instead, naming the directory
/// rather than the argument.
#[test]
#[cfg(unix)]
fn absolute_paths_resolve_through_symlinks_and_die_on_missing_directories() {
    let (repo, home) = fixture("abspath");
    let link = repo.parent().unwrap().join("repolink");
    std::os::unix::fs::symlink(&repo, &link).unwrap();

    let through_link = link.join("ign/inner/inner.txt");
    let out = run(
        &home,
        &repo,
        &["check-ignore", "-v", through_link.to_str().unwrap()],
    );
    assert_eq!(
        out_of(&out),
        format!(".gitignore:1:ign/\t{}\n", through_link.display())
    );
    assert_eq!(code(&out), 0);

    let out = run(
        &home,
        &repo,
        &["check-ignore", "-v", "/zvcs-no-such-root/deeper/x"],
    );
    assert_eq!(
        err_of(&out),
        "fatal: Invalid path '/zvcs-no-such-root': No such file or directory\n"
    );
    assert_eq!(code(&out), 128);

    // A missing *last* component is tolerated by `strbuf_realpath()`, so this
    // one reaches the ordinary "outside repository" refusal.
    let missing = repo.parent().unwrap().join("nope");
    let out = run(&home, &repo, &["check-ignore", "-v", missing.to_str().unwrap()]);
    assert!(
        err_of(&out).starts_with(&format!("fatal: {}: '{}' is outside repository at ", missing.display(), missing.display())),
        "stderr: {}",
        err_of(&out)
    );
    assert_eq!(code(&out), 128);
}
