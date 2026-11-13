//! Every command that edits a config file goes through config.c's store,
//! `repo_config_set_multivar_in_file_gently()` (config.c:3028-3288) and
//! `repo_config_copy_or_rename_section_in_file()` (config.c:3353-3545), which
//! copy the bytes they do not change verbatim and write only the changed lines
//! in their own format. A file that ends without a newline keeps that last line
//! unterminated, CRLF lines keep their CR, and comments, blank lines, duplicate
//! sections and `\` continuations survive where they stand.
//!
//! zvcs's `remote`, `branch`, `submodule` and `mv` re-serialised the whole file
//! through gitoxide's writer instead, which terminates the last line, writes a
//! changed value in place rather than as git's `\t<key> = <value>` line, and
//! rewrites sections it touches.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dst);
        } else {
            std::fs::copy(entry.path(), &dst).unwrap();
        }
    }
}

/// Run `args` under `bin` in a fresh copy of `fixture` at `work`, the same path for
/// both binaries so absolute paths the commands record agree, and return the output
/// with the bytes of each of `files` afterwards.
fn outcome(bin: &str, fixture: &Path, work: &Path, home: &Path, args: &[&str], files: &[&str]) -> ((String, String, i32), Vec<Vec<u8>>) {
    let _ = std::fs::remove_dir_all(work);
    copy_tree(fixture, work);
    let out = run(bin, &work.join("r"), home, args);
    let bytes = files.iter().map(|f| std::fs::read(work.join("r").join(f)).unwrap_or_default()).collect();
    (out, bytes)
}

fn compare(stock: &str, fixture: &Path, work: &Path, home: &Path, commands: &[&[&str]], files: &[&str]) {
    for args in commands {
        let ours = outcome(BIN, fixture, work, home, args, files);
        let theirs = outcome(stock, fixture, work, home, args, files);
        assert_eq!(
            ours.0, theirs.0,
            "{args:?}: output differs from stock ({})", fixture.display()
        );
        for (file, (a, b)) in files.iter().zip(ours.1.iter().zip(&theirs.1)) {
            assert_eq!(
                String::from_utf8_lossy(a),
                String::from_utf8_lossy(b),
                "{args:?}: {file} differs from stock ({})",
                fixture.display()
            );
        }
    }
}

/// `remote` and `branch` against a `.git/config` whose tail carries the shapes the
/// store copies through: a comment and blank line, a header with a trailing comment,
/// `key=value` without spaces, a duplicate section, a continuation line, and no final
/// newline — and the same shapes again with CRLF line ends.
#[test]
fn remote_and_branch_keep_untouched_config_bytes() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-config-store-bytes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let tails: [(&str, &str); 2] = [
        (
            "lf",
            "# top\n\n[remote \"o\"] ; c\n\turl=u0 # c\n[branch \"main\"]\n\tremote=o\n[remote \"o\"]\n\tfetch=+refs/heads/*:refs/remotes/o/*\n[foo]\n\tbar = a\\\n b\n[branch \"main\"]\n\tmerge=refs/heads/main\n[x]\n\ty = 1",
        ),
        (
            "crlf",
            "[remote \"o\"]\r\n\turl=u0\r\n\tfetch=+refs/heads/*:refs/remotes/o/*\r\n[branch \"main\"]\r\n\tremote=o\r\n\tmerge=refs/heads/main\r\n",
        ),
    ];
    let commands: [&[&str]; 11] = [
        &["remote", "add", "n", "u1"],
        &["remote", "rename", "o", "p"],
        &["remote", "remove", "o"],
        &["remote", "set-url", "o", "u9"],
        &["remote", "set-url", "--add", "o", "u9"],
        &["remote", "set-branches", "o", "main"],
        &["remote", "set-branches", "--add", "o", "topic"],
        &["branch", "-u", "o/main", "topic"],
        &["branch", "-t", "t2", "o/main"],
        &["branch", "--edit-description", "topic"],
        // `push -u` records the upstream with `install_branch_config()` (transport.c:121-123).
        &["-c", "maintenance.auto=false", "push", "-q", "-u", "../up.git", "topic"],
    ];
    for (tag, tail) in tails {
        let fixture = root.join(tag);
        let repo = fixture.join("r");
        std::fs::create_dir_all(&repo).unwrap();
        let (_, err, code) = run(stock, &fixture, &home, &["init", "-q", "--bare", "up.git"]);
        assert_eq!(code, 0, "{err}");
        for args in [
            &["init", "-q", "-b", "main", "."][..],
            &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "i"],
            &["branch", "topic"],
            &["update-ref", "refs/remotes/o/main", "HEAD"],
        ] {
            let (_, err, code) = run(stock, &repo, &home, args);
            assert_eq!(code, 0, "{args:?}: {err}");
        }
        let config = repo.join(".git/config");
        let mut bytes = std::fs::read(&config).unwrap();
        if tag == "crlf" {
            bytes = String::from_utf8(bytes).unwrap().replace('\n', "\r\n").into_bytes();
        }
        bytes.extend_from_slice(tail.as_bytes());
        std::fs::write(&config, bytes).unwrap();
        compare(stock, &fixture, &root.join("work"), &home, &commands, &[".git/config"]);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// `submodule` and `mv` of a submodule against a `.gitmodules` and `.git/config`
/// that end without a newline and carry a comment: `mv` rewrites
/// `submodule.<name>.path` in place (submodule.c:116-139), `set-branch` and
/// `set-url` write through `config_set_in_gitmodules_file_gently()`
/// (submodule-config.c:985-996), and `sync`, `deinit` and `add` edit `.git/config`
/// one key or section at a time.
#[test]
fn submodule_and_mv_keep_untouched_gitmodules_bytes() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-config-store-bytes-sub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let fixture = root.join("fixture");
    let up = fixture.join("up");
    let repo = fixture.join("r");
    std::fs::create_dir_all(&up).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let g = |dir: &Path, args: &[&str]| {
        let (_, err, code) = run(stock, dir, &home, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    };
    g(&up, &["init", "-q", "-b", "main", "."]);
    g(&up, &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "i"]);
    g(&repo, &["init", "-q", "-b", "main", "."]);
    g(&repo, &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "i"]);
    g(&repo, &["-c", "protocol.file.allow=always", "-c", "maintenance.auto=false", "submodule", "add", "-q", "../up", "sub"]);
    g(&repo, &["-c", "maintenance.auto=false", "commit", "-q", "-m", "s"]);
    for file in [".gitmodules", ".git/config"] {
        let path = repo.join(file);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"# c\n\n[foo] ; c\n  x=1 # c\n[bar]\n\ty = 1");
        std::fs::write(&path, bytes).unwrap();
    }
    g(&repo, &["add", ".gitmodules"]);

    let commands: [&[&str]; 7] = [
        &["mv", "sub", "sub2"],
        &["submodule", "set-branch", "-b", "dev", "sub"],
        &["submodule", "set-branch", "-d", "sub"],
        &["submodule", "set-url", "sub", "../up2"],
        &["submodule", "sync"],
        &["submodule", "deinit", "-f", "sub"],
        &["-c", "protocol.file.allow=always", "-c", "maintenance.auto=false", "submodule", "add", "../up", "s2"],
    ];
    compare(stock, &fixture, &root.join("work"), &home, &commands, &[".gitmodules", ".git/config"]);
    let _ = std::fs::remove_dir_all(&root);
}

/// `read_branches()` reads the configuration once and `config_read_branches()` warns
/// as it reads, so `more than one branch.<name>.remote` and `unhandled
/// branch.<name>.rebase` come out interleaved in configuration order
/// (builtin/remote.c:296-375), for `rename` and `rm` and for `show` and `prune`
/// through `get_remote_ref_states()` (:1155). And the store compiles a value-pattern
/// only for a file that exists (config.c:3078-3124): unsetting from a missing file
/// with an invalid pattern is `CONFIG_NOTHING_SET`, not `invalid pattern`.
#[test]
fn remote_branch_warnings_and_missing_file_pattern() {
    let Some(stock) = stock_git::stock_git() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("zvcs-config-store-warn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let fixture = root.join("fixture");
    let repo = fixture.join("r");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q", "-b", "main", "."][..],
        &["-c", "maintenance.auto=false", "commit", "-q", "--allow-empty", "-m", "i"],
        &["update-ref", "refs/remotes/o/main", "HEAD"],
    ] {
        let (_, err, code) = run(stock, &repo, &home, args);
        assert_eq!(code, 0, "{args:?}: {err}");
    }
    let config = repo.join(".git/config");
    let mut bytes = std::fs::read(&config).unwrap();
    bytes.extend_from_slice(
        b"[remote \"o\"]\n\turl=u0\n[branch \"b\"]\n\trebase=junk\n[branch \"main\"]\n\tremote=o\n\trebase=bad\n\tremote=o\n\tmerge=refs/heads/main\n[branch \"a\"]\n\tpushremote=o\n\tpushremote=o\n\tREBASE=zz\n",
    );
    std::fs::write(&config, bytes).unwrap();

    let commands: [&[&str]; 5] = [
        &["remote", "rename", "o", "p"],
        &["remote", "rm", "o"],
        &["remote", "show", "-n", "o"],
        &["remote", "prune", "-n", "o"],
        &["config", "-f", "missing", "--unset", "a.b", "["],
    ];
    compare(stock, &fixture, &root.join("work"), &home, &commands, &[".git/config", "missing"]);
    let _ = std::fs::remove_dir_all(&root);
}
