//! Two things stock git says that zvcs used to get wrong, both measured against
//! git 2.55.0 and reproduced here from scratch (no fixture repo, no network, no
//! `submodule add`, so this runs the same on a headless CI box).
//!
//! 1. `advice.ignoredHook` is a **two-line** hint naming the hook by the path
//!    `git_path()` builds — `.git/hooks/<event>`, however deep in the work tree
//!    the command ran (git has already chdir'd to the top). zvcs printed one
//!    line and spelled the path `./.git/…` from the root and `../.git/…` from a
//!    subdirectory, and ignored `GIT_ADVICE=0`.
//!
//! 2. `git submodule summary` renders a gitlink diff, and four of its shapes had
//!    no coverage at all: a typechange between a blob and a submodule, the
//!    `Warn: … doesn't contain commit` line for a commit the submodule lacks,
//!    the `--for-status` filter that drops `ignore = all` submodules (it was
//!    parsed and discarded), and the first-parent commit count, which counted
//!    one commit too many whenever one side was merged into the other off its
//!    first-parent chain.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A git invocation with the ambient environment pinned: this machine's
/// `~/.gitconfig` sets `core.commentChar`, and a stray `GIT_ADVICE` would
/// silently blank the hint half of this file.
fn cmd(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_ADVICE")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0200")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0200")
        .env("LC_ALL", "C");
    c
}

fn run(dir: &Path, args: &[&str]) -> Output {
    cmd(dir, args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn write(path: &Path, body: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

// ------------------------------------------------------- advice.ignoredHook ---

/// Stock git 2.55.0, hook file present but mode 644:
///
/// ```text
/// hint: The '.git/hooks/pre-commit' hook was ignored because it's not set as executable.
/// hint: You can disable this warning with `git config set advice.ignoredHook false`.
/// ```
#[test]
fn ignored_hook_hint_is_two_lines_naming_the_git_path() {
    let root = scratch("ignoredhook");
    let repo = root.join("r");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&repo, &["init", "-q", "-b", "main", "."]);
    write(&repo.join("a.txt"), "a\n");
    ok(&repo, &["add", "a.txt"]);

    let hook = repo.join(".git/hooks/pre-commit");
    write(&hook, "#!/bin/sh\nexit 0\n");
    // Deliberately not executable: that is the whole trigger.
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();

    let expected = "hint: The '.git/hooks/pre-commit' hook was ignored because it's not set as executable.\n\
                    hint: You can disable this warning with `git config set advice.ignoredHook false`.\n";

    let out = run(&repo, &["commit", "-m", "one"]);
    assert!(out.status.success(), "commit failed: {}", stderr_of(&out));
    assert_eq!(stderr_of(&out), expected, "hint from the work tree root");

    // `setup.c` moves to the top of the work tree before anything is printed, so
    // the depth of the invocation cannot change the path in the message.
    let sub = repo.join("deep/deeper");
    std::fs::create_dir_all(&sub).unwrap();
    write(&sub.join("b.txt"), "b\n");
    ok(&sub, &["add", "b.txt"]);
    let out = run(&sub, &["commit", "-m", "two"]);
    assert!(out.status.success(), "commit failed: {}", stderr_of(&out));
    assert_eq!(stderr_of(&out), expected, "hint from two directories down");

    // Both switches git honours, neither of which zvcs used to consult here.
    write(&repo.join("c.txt"), "c\n");
    ok(&repo, &["add", "c.txt"]);
    let out = run(&repo, &["-c", "advice.ignoredHook=false", "commit", "-m", "three"]);
    assert_eq!(stderr_of(&out), "", "advice.ignoredHook=false must silence it");

    // `git_env_bool` runs the value through `git_parse_maybe_bool`: the spelled
    // out booleans compare without regard to case, and an integer in any base
    // `strtoimax` accepts counts as one.
    for (i, falsy) in ["0", "false", "FALSE", "No", "off", ""].iter().enumerate() {
        let name = format!("e{i}.txt");
        write(&repo.join(&name), "e\n");
        ok(&repo, &["add", &name]);
        let out = cmd(&repo, &["commit", "-m", "four"])
            .env("GIT_ADVICE", falsy)
            .output()
            .unwrap();
        assert_eq!(stderr_of(&out), "", "GIT_ADVICE={falsy:?} must silence the hint");
    }

    // A value that is not a boolean at all is `die()`, not a default: silently
    // treating it as "advice on" hides a typo in a caller's environment.
    write(&repo.join("f.txt"), "f\n");
    ok(&repo, &["add", "f.txt"]);
    let out = cmd(&repo, &["commit", "-m", "five"])
        .env("GIT_ADVICE", "bogus")
        .output()
        .unwrap();
    assert_eq!(
        stderr_of(&out),
        "fatal: bad boolean environment value 'bogus' for 'GIT_ADVICE'\n"
    );
    assert_eq!(out.status.code(), Some(128), "git dies with 128 here");
}

// ----------------------------------------------------- submodule summary ------

/// A superproject with one nested repository per shape `submodule summary` can
/// render, assembled with `update-index --cacheinfo` so no clone or network is
/// involved. Returns the superproject path.
fn build_superproject(root: &Path) -> std::path::PathBuf {
    let sup = root.join("super");
    std::fs::create_dir_all(&sup).unwrap();
    ok(&sup, &["init", "-q", "-b", "main", "."]);
    write(&sup.join("base.txt"), "base\n");
    ok(&sup, &["add", "base.txt"]);

    let nest = |name: &str, commits: usize| {
        let dir = sup.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        ok(&dir, &["init", "-q", "-b", "main", "."]);
        for i in 1..=commits {
            write(&dir.join("f.txt"), &format!("{name} {i}\n"));
            ok(&dir, &["add", "f.txt"]);
            ok(&dir, &["commit", "-q", "-m", &format!("{name} c{i}")]);
        }
        dir
    };
    for (name, n) in [
        ("sm-mod", 2),
        ("sm-ign", 2),
        ("sm-miss", 1),
        ("sm-del", 1),
        ("sm-blob", 1),
        ("toggle", 1),
    ] {
        nest(name, n);
    }

    // `sm-merge`: the side branch is an ancestor of the merge commit but is not
    // on its first-parent chain, so a set comparison of the two chains counts it
    // and `rev-list --first-parent A...B` does not.
    let merge = nest("sm-merge", 1);
    ok(&merge, &["checkout", "-q", "-b", "side"]);
    write(&merge.join("s.txt"), "s1\n");
    ok(&merge, &["add", "s.txt"]);
    ok(&merge, &["commit", "-q", "-m", "merge s1"]);
    let side = ok(&merge, &["rev-parse", "HEAD"]).trim().to_string();
    ok(&merge, &["checkout", "-q", "main"]);
    write(&merge.join("f.txt"), "m2\n");
    ok(&merge, &["add", "f.txt"]);
    ok(&merge, &["commit", "-q", "-m", "merge c2"]);
    ok(&merge, &["merge", "-q", "--no-ff", "-m", "merge m", "side"]);

    write(
        &sup.join(".gitmodules"),
        "[submodule \"sm-mod\"]\n\tpath = sm-mod\n\turl = ./sm-mod\n\
         [submodule \"sm-ign\"]\n\tpath = sm-ign\n\turl = ./sm-ign\n\tignore = all\n\
         [submodule \"sm-miss\"]\n\tpath = sm-miss\n\turl = ./sm-miss\n\
         [submodule \"sm-del\"]\n\tpath = sm-del\n\turl = ./sm-del\n\
         [submodule \"sm-blob\"]\n\tpath = sm-blob\n\turl = ./sm-blob\n\
         [submodule \"sm-merge\"]\n\tpath = sm-merge\n\turl = ./sm-merge\n",
    );
    ok(&sup, &["add", ".gitmodules"]);

    let rev = |name: &str, spec: &str| {
        ok(&sup.join(name), &["rev-parse", spec]).trim().to_string()
    };
    let link = |name: &str, oid: &str| {
        ok(
            &sup,
            &["update-index", "--add", "--cacheinfo", &format!("160000,{oid},{name}")],
        );
    };
    link("sm-mod", &rev("sm-mod", "HEAD~1"));
    link("sm-ign", &rev("sm-ign", "HEAD~1"));
    link("sm-miss", &rev("sm-miss", "HEAD"));
    link("sm-del", &rev("sm-del", "HEAD"));
    link("sm-blob", &rev("sm-blob", "HEAD"));
    link("sm-merge", &side);

    // `toggle` is a plain file in the commit and a gitlink in the index.
    write(&sup.join("toggle-file"), "i am a file\n");
    let blob = ok(&sup, &["hash-object", "-w", "toggle-file"]).trim().to_string();
    ok(
        &sup,
        &["update-index", "--add", "--cacheinfo", &format!("100644,{blob},toggle")],
    );
    std::fs::remove_file(sup.join("toggle-file")).unwrap();
    ok(&sup, &["commit", "-q", "-m", "superproject"]);

    // Index side of the diff.
    let toggle_head = rev("toggle", "HEAD");
    ok(
        &sup,
        &["update-index", "--cacheinfo", &format!("160000,{toggle_head},toggle")],
    );
    write(&sup.join("sm-blob-file"), "now a file\n");
    let blob = ok(&sup, &["hash-object", "-w", "sm-blob-file"]).trim().to_string();
    ok(&sup, &["update-index", "--force-remove", "sm-blob"]);
    ok(
        &sup,
        &["update-index", "--add", "--cacheinfo", &format!("100644,{blob},sm-blob")],
    );
    std::fs::remove_file(sup.join("sm-blob-file")).unwrap();
    std::fs::remove_dir_all(sup.join("sm-blob")).unwrap();
    write(&sup.join("sm-blob"), "now a file\n");
    ok(&sup, &["update-index", "--force-remove", "sm-del"]);
    ok(
        &sup,
        &[
            "update-index",
            "--cacheinfo",
            "160000,0123456789012345678901234567890123456789,sm-miss",
        ],
    );
    sup
}

/// The `* <path> …` header of the row for `path`, plus every line under it up to
/// the blank separator.
fn row<'a>(body: &'a str, path: &str) -> &'a str {
    let head = format!("* {path} ");
    let start = body
        .find(&head)
        .unwrap_or_else(|| panic!("no row for {path} in:\n{body}"));
    let rest = &body[start..];
    match rest.find("\n\n") {
        Some(end) => &rest[..end + 1],
        None => rest,
    }
}

#[test]
fn summary_renders_typechanges_missing_commits_and_first_parent_counts() {
    let root = scratch("smsummary");
    let sup = build_superproject(&root);
    // Every id comes out of the superproject, because two of these paths are no
    // longer directories by now.
    let short = |spec: &str| ok(&sup, &["rev-parse", spec]).trim()[..7].to_string();
    let staged = ok(&sup, &["submodule", "summary", "--cached", "HEAD"]);

    // A submodule that became a file: both sides abbreviated, no commit count,
    // and no log — `total_commits` stays -1 because the source cannot be
    // verified inside a path that is no longer a directory.
    assert_eq!(
        row(&staged, "sm-blob"),
        format!(
            "* sm-blob {}(submodule)->{}(blob):\n",
            short("HEAD:sm-blob"),
            short(":sm-blob")
        ),
        "gitlink -> blob typechange"
    );

    // A file that became a submodule: the count and the one-line log of the new
    // submodule's tip.
    assert_eq!(
        row(&staged, "toggle"),
        format!(
            "* toggle {}(blob)->{}(submodule) (1):\n  > toggle c1\n",
            short("HEAD:toggle"),
            short(":toggle")
        ),
        "blob -> gitlink typechange"
    );

    // The commit the submodule does not have: a `Warn:` line in place of the log,
    // the full 40-hex id, and no `(<n>)` on the header.
    assert_eq!(
        row(&staged, "sm-miss"),
        format!(
            "* sm-miss {}...0123456:\n  \
             Warn: sm-miss doesn't contain commit 0123456789012345678901234567890123456789\n",
            short("HEAD:sm-miss")
        ),
        "unverifiable destination commit"
    );

    // A gitlink dropped from the index while its checkout stays on disk is still
    // a removal — the index, not the work tree, decides.
    assert_eq!(
        row(&staged, "sm-del"),
        format!("* sm-del {}...0000000:\n", short("HEAD:sm-del")),
        "gitlink removed from the index"
    );

    // `rev-list --first-parent A...B` excludes anything the other side reaches at
    // all, so the side branch merged into `B` is not counted even though it is
    // absent from B's first-parent chain. Counting chain membership yields 3 and
    // an extra `< merge s1` line.
    let worktree = ok(&sup, &["submodule", "summary"]);
    let merge_head = ok(&sup.join("sm-merge"), &["rev-parse", "HEAD"]).trim()[..7].to_string();
    assert_eq!(
        row(&worktree, "sm-merge"),
        format!(
            "* sm-merge {}...{merge_head} (2):\n  > merge m\n  > merge c2\n",
            short("HEAD:sm-merge")
        ),
        "first-parent count must exclude commits the other side merged in"
    );

    // `diff-index` reports a path whose index entry differs from the tree even
    // when the work tree has brought the two ends back together, which is the
    // only way a `(0)` row is produced.
    assert_eq!(
        row(&worktree, "sm-miss"),
        format!("* sm-miss {0}...{0} (0):\n", short("HEAD:sm-miss")),
        "index-only difference still makes a row"
    );
}

#[test]
fn for_status_drops_ignore_all_submodules() {
    let root = scratch("smforstatus");
    let sup = build_superproject(&root);

    let plain = ok(&sup, &["submodule", "summary", "--files"]);
    assert!(
        plain.contains("* sm-ign "),
        "without --for-status an ignore=all submodule is listed:\n{plain}"
    );
    assert!(plain.contains("* sm-mod "), "control row missing:\n{plain}");

    let filtered = ok(&sup, &["submodule", "summary", "--files", "--for-status"]);
    assert!(
        !filtered.contains("* sm-ign "),
        "--for-status must drop submodule.sm-ign.ignore=all:\n{filtered}"
    );
    assert!(
        filtered.contains("* sm-mod "),
        "--for-status must keep everything else:\n{filtered}"
    );

    // The superproject config outranks `.gitmodules`, so turning the setting off
    // there brings the row back.
    let unignored = ok(
        &sup,
        &[
            "-c",
            "submodule.sm-ign.ignore=none",
            "submodule",
            "summary",
            "--files",
            "--for-status",
        ],
    );
    assert!(
        unignored.contains("* sm-ign "),
        "submodule.<name>.ignore in config must win over .gitmodules:\n{unignored}"
    );
}
