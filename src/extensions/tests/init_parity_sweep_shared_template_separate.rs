//! `git init` details a differential sweep found, each measured against stock git
//! 2.56 on twin fixtures and compared on stdout, stderr, exit code and the
//! resulting tree (modes and the small text files of the git directory).
//!
//! * `--shared` adjusts only what `init_db()` creates, plus the git directory and
//!   its `refs/` (setup.c:2608-2610, refs/files-backend.c:3679-3680): a
//!   reinitialization left `hooks/`, `objects/` and existing files alone where
//!   zvcs opened up the whole tree.
//! * `core.sharedRepository` is read from every scope (setup.c:2588-2590), and a
//!   rewritten assignment takes `repo_config_set()`'s spelling.
//! * `init.templateDir` comes from the protected configuration
//!   (setup.c:2275-2297): a `-c` value reaches a reinitialization, an empty value
//!   copies nothing, and the repository's own `config` is not consulted.
//! * `copy_file()` creates a template copy `0666`/`0777` under the umask
//!   (copy.c:36-64) instead of keeping the template's own bits.
//! * `--separate-git-dir` naming an existing directory initializes inside it, and
//!   one naming a regular file is `read_gitfile()`'s `invalid gitfile format`.
//! * A valueless `init.defaultBranch` dies before `HEAD` and `objects/` exist.
//! * `core.logAllRefUpdates` and `core.precomposeunicode` set in another scope are
//!   not written into the new repository (setup.c:2636-2637,
//!   compat/precompose_utf8.c:58-59).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A fresh directory holding the fixture every scenario starts from: a
/// repository `existing` with one commit, a bare `existing.git`, a template
/// `tmpl`, a non-empty directory and a regular file.
fn fixture(stock: &str, tag: &str, who: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-init-sweep-{tag}-{who}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let script = r#"
        set -e
        "$STOCK" init -q -b main existing
        echo e > existing/e
        "$STOCK" -C existing add e
        "$STOCK" -C existing -c maintenance.auto=false commit -q -m e
        "$STOCK" init -q --bare -b main existing.git
        mkdir -p tmpl/hooks tmpl/info nonempty
        printf '#!/bin/sh\n' > tmpl/hooks/pre-commit
        chmod 700 tmpl/hooks/pre-commit
        echo desc > tmpl/description
        chmod 600 tmpl/description
        echo z > nonempty/z
        echo file > afile
    "#;
    let status = Command::new("sh")
        .args(["-c", script])
        .current_dir(&root)
        .envs(env(&root))
        .env("STOCK", stock)
        .status()
        .unwrap();
    assert!(status.success());
    root
}

fn env(root: &Path) -> Vec<(&'static str, String)> {
    vec![
        // Outside the compared tree: zvcs keeps a cache under its home.
        ("HOME", format!("{}.home", root.display())),
        ("ZVCS_HOME", format!("{}.home", root.display())),
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_CEILING_DIRECTORIES", root.display().to_string()),
        ("GIT_AUTHOR_NAME", "A".into()),
        ("GIT_AUTHOR_EMAIL", "a@example.com".into()),
        ("GIT_COMMITTER_NAME", "C".into()),
        ("GIT_COMMITTER_EMAIL", "c@example.com".into()),
        ("GIT_AUTHOR_DATE", "1700000000 +0000".into()),
        ("GIT_COMMITTER_DATE", "1700000000 +0000".into()),
        ("GIT_EDITOR", "true".into()),
        ("LC_ALL", "C".into()),
    ]
}

/// Every path under `root` with its permission bits, and the contents of the
/// small text files a git directory keeps.
fn dump(root: &Path) -> String {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        use std::os::unix::fs::PermissionsExt;
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            let mode = meta.permissions().mode() & 0o7777;
            out.push(format!("{mode:04o} {rel}"));
            let name = path.file_name().unwrap().to_string_lossy();
            let text = meta.is_file()
                && (matches!(name.as_ref(), "config" | "HEAD" | "description" | ".git" | "alternates")
                    || rel.contains("hooks/"));
            if text {
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                out.push(body.replace(&root.display().to_string(), "<ROOT>"));
            }
            if meta.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.join("\n")
}

/// Runs each scenario through both binaries on twin fixtures and compares.
fn compare(tag: &str, scenarios: &[&str]) {
    let Some(stock) = stock_git() else { return };
    for (i, script) in scenarios.iter().enumerate() {
        let mut seen = Vec::new();
        for (who, bin) in [("stock", stock), ("zvcs", BIN)] {
            let root = fixture(stock, &format!("{tag}{i}"), who);
            let out = Command::new("sh")
                .args(["-c", script])
                .current_dir(&root)
                .envs(env(&root))
                .env("GIT", bin)
                .env("ROOT", &root)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_TEMPLATE_DIR")
                .output()
                .unwrap();
            let scrub = |b: &[u8]| String::from_utf8_lossy(b).replace(&root.display().to_string(), "<ROOT>");
            seen.push((scrub(&out.stdout), scrub(&out.stderr), out.status.code(), dump(&root)));
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_dir_all(format!("{}.home", root.display()));
        }
        if seen[1] != seen[0] {
            let (a, b) = (format!("{:?}", seen[0]), format!("{:?}", seen[1]));
            // The first divergence and its surroundings, rather than two whole trees.
            let at = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
            let near = |s: &str| s.chars().skip(at.saturating_sub(200)).take(400).collect::<String>();
            panic!("scenario: {script}\nstock: {}\nzvcs:  {}", near(&a), near(&b));
        }
    }
}

#[test]
fn shared_reinit_opens_up_only_the_git_dir_and_its_refs() {
    compare(
        "shared",
        &[
            r#""$GIT" init --shared=group existing"#,
            r#""$GIT" init -q --shared=0640 existing.git"#,
            r#""$GIT" -c core.sharedRepository=group init -b main new"#,
            r#""$GIT" -C existing config core.sharedRepository group && "$GIT" init existing"#,
            r#"mkdir -p ex/.git/hooks && "$GIT" init -q -b main --shared=group ex"#,
        ],
    );
}

#[test]
fn templates_come_from_the_protected_config_and_copy_under_the_umask() {
    compare(
        "template",
        &[
            r#""$GIT" -c init.templateDir="$ROOT/tmpl" init existing"#,
            r#""$GIT" -c init.templateDir= init -q -b main new"#,
            r#""$GIT" -C existing config init.templateDir "$ROOT/tmpl" && "$GIT" init -q existing"#,
            r#""$GIT" init -q -b main --template=tmpl new"#,
            r#"umask 077 && "$GIT" init -q -b main --template=tmpl new"#,
        ],
    );
}

#[test]
fn separate_git_dir_takes_an_existing_directory_and_refuses_a_plain_file() {
    compare(
        "separate",
        &[
            r#""$GIT" init -b main --separate-git-dir=nonempty new"#,
            r#""$GIT" init --separate-git-dir=existing.git new"#,
            r#""$GIT" init -b main --separate-git-dir=afile new"#,
            r#"printf 'gitdir: existing.git\n' > gf && "$GIT" init --separate-git-dir=gf new"#,
        ],
    );
}

#[test]
fn defaults_another_scope_sets_are_not_written() {
    compare(
        "defaults",
        &[
            r#""$GIT" -c init.defaultBranch init new"#,
            r#""$GIT" -c core.logAllRefUpdates=false -c core.precomposeunicode=false init -q -b main new"#,
            r#""$GIT" -c core.logAllRefUpdates=false init -q existing"#,
        ],
    );
}
