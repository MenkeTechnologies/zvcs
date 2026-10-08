//! The order in which `init`, `init-db`, `history`, `unpack-file`, `hash-object`,
//! `multi-pack-index`, `index-pack` and `fsck` read configuration and check their operands,
//! against stock git.
//!
//! * `cmd_init_db()` parses its options, enters (and creates) the operand directory, checks
//!   `--object-format` / `--ref-format` and only then reads configuration — the `core.*` keys
//!   first, the rest of `git_default_config()` after the templates were copied — from the
//!   repository it is creating, never from the one the cwd sits in.
//! * `cmd_history_*()` count their operands before `repo_config(git_default_config)`, and the
//!   diff callback is read by the replay, after the identity and root-commit checks.
//! * `cmd_unpack_file()` resolves its operand before it reads any configuration.
//! * `hash-object` outside a repository (here: `$GIT_OBJECT_DIRECTORY` names nothing, so no
//!   directory is a git directory) never reads the repository's config files.
//! * `multi-pack-index` resolves `--object-dir` the moment it is read and refuses a directory
//!   that is not one of the repository's object directories; `compact` checks
//!   `--no-write-chain-file` against `--incremental`.
//! * `index-pack --index-version=` is read with `strtoul()`, which skips leading white space.
//! * `fsck <object>` dies on a loose object that exists but will not inflate.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

type Outcome = (String, String, Option<i32>);

fn command(bin: &str, dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env("LC_ALL", "C")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@x")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd
}

/// stdout, stderr and exit status, with the sandbox root spelled `ROOT`.
fn run(bin: &str, root: &Path, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Outcome {
    let out = command(bin, cwd, args, envs).output().unwrap();
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let text = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .replace(&canonical.display().to_string(), "ROOT")
            .replace(&root.display().to_string(), "ROOT")
    };
    (text(&out.stdout), text(&out.stderr), out.status.code())
}

/// Every path under `dir` (sorted, relative), `hooks/` content left out.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(base).unwrap().display().to_string();
            if rel.ends_with("hooks") || rel.contains("hooks/") {
                continue;
            }
            out.push(rel);
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn sandbox(tag: &str, which: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-order-{tag}-{which}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Run `args` under both binaries, each in a fresh sandbox prepared by `setup(stock, root)`,
/// from `cwd_rel` inside it; output and the resulting file tree must agree.
fn compare(
    stock: &str,
    tag: &str,
    setup: &dyn Fn(&str, &Path),
    cwd_rel: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) {
    let (s, z) = (sandbox(tag, "stock"), sandbox(tag, "zvcs"));
    for root in [&s, &z] {
        setup(stock, root);
    }
    let want = run(stock, &s, &s.join(cwd_rel), args, envs);
    let got = run(BIN, &z, &z.join(cwd_rel), args, envs);
    assert_eq!(got, want, "{args:?} {envs:?}");
    assert_eq!(tree(&z), tree(&s), "tree after {args:?}");
    let _ = std::fs::remove_dir_all(&s);
    let _ = std::fs::remove_dir_all(&z);
}

fn nothing(_: &str, _: &Path) {}

/// A repository with two commits on `main`.
fn repo(stock: &str, root: &Path) {
    let git = |args: &[&str]| {
        let out = command(stock, root, args, &[]).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q", "-b", "main"]);
    for (name, body) in [("a", "1\n"), ("b", "2\n")] {
        std::fs::write(root.join(name), body).unwrap();
        git(&["add", name]);
        git(&["commit", "-qm", name]);
    }
}

fn append_config(root: &Path, text: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(root.join(".git/config")).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

#[test]
fn init_reads_configuration_after_options_operand_and_formats() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let bad_push = ["-c", "push.default=bogus"];
    let bad_core = ["-c", "core.safecrlf=bogus"];
    let cases: Vec<Vec<&str>> = vec![
        // A usage error or an unknown option beats a refused value.
        [&bad_push[..], &["init", ".", "."]].concat(),
        [&bad_push[..], &["init", "--bogus"]].concat(),
        [&bad_push[..], &["init-db", "a", "b"]].concat(),
        // The operand directory is created before the configuration is read, and
        // `--object-format` is checked before it too.
        [&bad_core[..], &["init", "-q", "--quiet", "newrepo"]].concat(),
        [&bad_core[..], &["init", "--bare", "newrepo"]].concat(),
        [&bad_push[..], &["init", "-q", "--object-format=bogus", "x"]].concat(),
        // A key outside `core.*` is refused only once the templates are in place.
        [&bad_push[..], &["init", "a"]].concat(),
        [&bad_push[..], &["init"]].concat(),
        [&bad_push[..], &["init", "--bare", "bb"]].concat(),
    ];
    for (i, args) in cases.iter().enumerate() {
        compare(stock, &format!("init{i}"), &nothing, "", args, &[]);
    }
}

#[test]
fn init_reads_the_repository_it_creates_not_the_one_it_stands_in() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    // The surrounding repository's config is unreadable; a new one in a subdirectory never
    // looks at it ...
    let broken = |stock: &str, root: &Path| {
        repo(stock, root);
        append_config(root, "garbage line\n");
    };
    compare(stock, "nested1", &broken, "", &["init", "nested/dir"], &[]);
    compare(stock, "nested2", &broken, "", &["init-db", "-q", "n"], &[]);
    // ... while re-initialising it in place reads it, for syntax and for values.
    compare(stock, "same1", &broken, "", &["init"], &[]);
    let bad_core = |stock: &str, root: &Path| {
        repo(stock, root);
        append_config(root, "[core]\n\tbare = bogus\n");
    };
    compare(stock, "same2", &bad_core, "", &["init"], &[]);
    compare(stock, "nested3", &bad_core, "", &["init", "sub"], &[]);
    let bad_push = |stock: &str, root: &Path| {
        repo(stock, root);
        append_config(root, "[push]\n\tdefault = bogus\n");
    };
    compare(stock, "same3", &bad_push, "", &["init"], &[]);
    compare(stock, "nested4", &bad_push, "", &["init", "sub"], &[]);
}

#[test]
fn history_counts_operands_before_reading_configuration() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let bad_push = ["-c", "push.default=bogus"];
    let cases: Vec<Vec<&str>> = vec![
        [&bad_push[..], &["history", "reword"]].concat(),
        [&bad_push[..], &["history", "reword", "HEAD", "HEAD"]].concat(),
        [&bad_push[..], &["history", "drop"]].concat(),
        [&bad_push[..], &["history", "split"]].concat(),
        [&bad_push[..], &["history", "reword", "--bogus", "HEAD"]].concat(),
        [&bad_push[..], &["history", "reword", "--update-refs=zz", "HEAD"]].concat(),
        [&bad_push[..], &["history", "fixup", "HEAD", "--empty=zz"]].concat(),
        [&bad_push[..], &["history", "--bogus"]].concat(),
        [&bad_push[..], &["history", "bogus"]].concat(),
        [&bad_push[..], &["history"]].concat(),
        [&bad_push[..], &["history", "reword", "-h"]].concat(),
        // With its operands in order the configuration is read, before the commit is looked up.
        [&bad_push[..], &["history", "reword", "HEAD"]].concat(),
        [&bad_push[..], &["history", "reword", "nonexist"]].concat(),
        // The settings block comes after the lookup.
        vec!["-c", "core.packedGitLimit=bogus", "history", "reword", "nonexist"],
        vec!["-c", "core.packedGitLimit=bogus", "history", "reword", "HEAD"],
        // The diff callback is read by the replay: after a missing commit, a root commit or
        // nothing staged, but before the rewrite lands.
        vec!["-c", "diff.renameLimit=bogus", "history", "reword", "nonexist"],
        vec!["-c", "diff.renameLimit=bogus", "history", "reword", "HEAD~1"],
        vec!["-c", "diff.renameLimit=bogus", "history", "drop", "HEAD~1"],
        vec!["-c", "diff.renameLimit=bogus", "history", "fixup", "HEAD"],
    ];
    for (i, args) in cases.iter().enumerate() {
        compare(stock, &format!("hist{i}"), &repo, "", args, &[]);
    }
}

#[test]
fn unpack_file_resolves_its_operand_before_reading_configuration() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let bad_push = ["-c", "push.default=bogus"];
    let cases: Vec<Vec<&str>> = vec![
        [&bad_push[..], &["unpack-file"]].concat(),
        [&bad_push[..], &["unpack-file", "a", "b"]].concat(),
        [&bad_push[..], &["unpack-file", "-h", "HEAD:a", "-h"]].concat(),
        [&bad_push[..], &["unpack-file", "nonexistent"]].concat(),
        // A name that resolves reaches the configuration.
        [&bad_push[..], &["unpack-file", "HEAD:a"]].concat(),
    ];
    for (i, args) in cases.iter().enumerate() {
        compare(stock, &format!("unpack{i}"), &repo, "", args, &[]);
    }
}

#[test]
fn a_command_that_found_no_repository_leaves_its_config_alone() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    // `$GIT_OBJECT_DIRECTORY` naming nothing un-recognises every repository on the way up, so
    // `hash-object` (gentle setup) runs outside one and never reads `.git/config`.
    let broken = |stock: &str, root: &Path| {
        repo(stock, root);
        append_config(root, "[core]\n\tworktree\n");
    };
    let envs = [("GIT_OBJECT_DIRECTORY", "/nonexistent-zvcs-objects")];
    for (i, cwd) in ["", ".git", ".git/info"].iter().enumerate() {
        compare(stock, &format!("nongit{i}"), &broken, cwd, &["hash-object", "--stdin"], &envs);
        compare(stock, &format!("nongitp{i}"), &broken, cwd, &["hash-object", "--path=x", "--path="], &envs);
    }
    // Strict setup (`-w`) still dies, and so does a command that reads the config normally.
    compare(stock, "nongitw", &broken, ".git/info", &["hash-object", "-w", "--stdin"], &envs);
    // Standing inside the git directory, the message names the file `./config`.
    compare(stock, "inside1", &broken, ".git/info", &["status"], &[]);
    compare(stock, "inside2", &broken, ".git/info", &["hash-object", "--stdin"], &[]);
    compare(stock, "inside3", &broken, ".git", &["status"], &[]);
}

#[test]
fn multi_pack_index_object_dir_is_resolved_and_checked() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("", vec!["multi-pack-index", "--object-dir=.git/objects", "write"]),
        ("", vec!["multi-pack-index", "--object-dir=objects", "write"]),
        ("", vec!["multi-pack-index", "--object-dir=nonexist", "write"]),
        ("", vec!["multi-pack-index", "--object-dir=nonexist/x", "write"]),
        ("", vec!["multi-pack-index", "--object-dir", "nonexist", "expire"]),
        ("", vec!["multi-pack-index", "write", "--object-dir=nonexist"]),
        ("", vec!["multi-pack-index", "verify", "--object-dir=nonexist"]),
        ("", vec!["multi-pack-index", "--object-dir=nonexist", "--bogus"]),
        ("", vec!["multi-pack-index", "--no-object-dir", "write"]),
        // Resolved against the process's working directory, in the order the options come.
        (".git", vec!["multi-pack-index", "--object-dir=.git/objects", "write"]),
        (".git/refs/heads", vec!["multi-pack-index", "--object-dir=.git/objects", "--no-incremental"]),
        (".git/refs/heads", vec!["multi-pack-index", "--object-dir=../../objects", "write"]),
        // `compact`: the chain-file flag needs `--incremental`, whichever spelling came last.
        ("", vec!["multi-pack-index", "compact", "--no-write-chain-file", "a", "b"]),
        ("", vec!["multi-pack-index", "compact", "expire", "repack", "--no-write-chain-file", "--no-incremental"]),
        ("", vec!["multi-pack-index", "compact", "--incremental", "--no-write-chain-file", "a", "b"]),
        ("", vec!["multi-pack-index", "compact", "--no-write-chain-file", "--incremental", "--write-chain-file", "a", "b"]),
        ("", vec!["multi-pack-index", "compact", "--no-write-chain-file", "a"]),
    ];
    for (i, (cwd, args)) in cases.iter().enumerate() {
        compare(stock, &format!("midx{i}"), &repo, cwd, args, &[]);
    }
}

#[test]
fn index_pack_index_version_is_read_with_strtoul() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    // A trailing `--bogus` makes every accepted value end in the usage block, so the cases
    // that differ are exactly the ones `strtoul()` refuses (`fatal: bad --index-version=`).
    for (i, value) in [
        "--index-version= 1",
        "--index-version=\t2",
        "--index-version=+2",
        "--index-version=-0",
        "--index-version= ",
        "--index-version=2, 0x10",
        "--index-version=2,0x",
        "--index-version=2,010",
        "--index-version=2,0x80000000",
        "--index-version=3",
        "--index-version=1,0xg",
    ]
    .iter()
    .enumerate()
    {
        compare(stock, &format!("ixv{i}"), &repo, "", &["index-pack", value, "--bogus"], &[]);
    }
}

#[test]
fn fsck_dies_on_a_corrupt_loose_object_named_on_the_command_line() {
    let Some(stock) = stock_git::stock_git_at_least((2, 56, 0)) else { return };
    let corrupt = |stock: &str, root: &Path| {
        repo(stock, root);
        let dir = root.join(".git/objects/ab");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("1234567890123456789012345678901234abcd"), b"not zlib data").unwrap();
    };
    let id = "ab1234567890123456789012345678901234abcd";
    for (i, cwd) in ["", ".git/info"].iter().enumerate() {
        compare(stock, &format!("fsck{i}"), &corrupt, cwd, &["fsck", "--no-reflogs", "nonsense", id, "HEAD"], &[]);
    }
}
