//! `git clone` details a differential sweep over local sources found, each
//! measured against stock git 2.56 on twin fixtures and compared on stdout,
//! stderr, exit code and the resulting tree (modes and the small text files of
//! the git directory).
//!
//! * `copy_or_link_directory()` copies with `copy_file_with_time(..., 0666)`
//!   (builtin/clone.c:338), so `--no-hardlinks` leaves loose objects `0644`
//!   under the usual umask, not the source's `0444`.
//! * A pack the transport received is `index-pack`'s, `0444` under the umask
//!   (builtin/index-pack.c:365-366, odb.c:72-84), where zvcs left gitoxide's
//!   `0600`.
//! * `-s` records `get_repo_path()`'s spelling of the source, `absolute_pathdup()`
//!   of what was typed (builtin/clone.c:144-155, :350-357), so `./src` stays
//!   `<cwd>/./src/.git/objects`.
//! * The template is `get_template_dir()`'s (setup.c:2275-2297), so
//!   `GIT_TEMPLATE_DIR` and `init.templateDir` reach a clone, and a template's
//!   `config` is where the clone's configuration starts.
//! * `clone.defaultRemoteName` is validated like `-o` (builtin/clone.c:1284-1292),
//!   and a valueless one dies before anything is made (builtin/clone.c:750-754).

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;
use stock_git::stock_git;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// A fresh directory holding a source repository `src` with two commits and a
/// branch, and a template `tmpl` whose `config` carries a key of its own.
fn fixture(stock: &str, tag: &str, who: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-clone-sweep-{tag}-{who}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let script = r#"
        set -e
        "$STOCK" init -q -b main src
        echo a > src/a
        "$STOCK" -C src add a
        "$STOCK" -C src -c maintenance.auto=false commit -q -m one
        "$STOCK" -C src branch side
        echo b >> src/a
        "$STOCK" -C src -c maintenance.auto=false commit -q -a -m two
        mkdir -p tmpl/hooks
        printf '#!/bin/sh\n' > tmpl/hooks/pre-commit
        chmod 755 tmpl/hooks/pre-commit
        printf '[core]\n\tfoo = bar\n' > tmpl/config
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

/// Every path of the clone `dst` with its permission bits — a pack under its
/// extension alone, as the two implementations name packs differently — and
/// the contents of the small text files a git directory keeps.
fn dump(root: &Path) -> String {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        use std::os::unix::fs::PermissionsExt;
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let mut rel = path.strip_prefix(root).unwrap().display().to_string();
            if rel.contains("objects/pack/pack-") {
                rel = format!("pack.{}", path.extension().unwrap().to_string_lossy());
            }
            let mode = meta.permissions().mode() & 0o7777;
            out.push(format!("{mode:04o} {rel}"));
            let name = path.file_name().unwrap().to_string_lossy();
            if meta.is_file() && matches!(name.as_ref(), "config" | "HEAD" | "alternates") {
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                out.push(body.replace(&root.display().to_string(), "<ROOT>"));
            }
            if meta.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    let dst = root.join("dst");
    if dst.exists() {
        walk(root, &dst, &mut out);
    }
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
fn copied_and_fetched_objects_take_git_modes() {
    compare(
        "modes",
        &[
            r#""$GIT" -c maintenance.auto=false clone -q --no-hardlinks src dst"#,
            r#""$GIT" -c maintenance.auto=false clone -q --no-local src dst"#,
            r#"umask 077 && "$GIT" -c maintenance.auto=false clone -q "file://$ROOT/src" dst"#,
        ],
    );
}

#[test]
fn shared_records_the_source_as_typed() {
    compare("shared", &[r#""$GIT" clone -q -s ./src dst"#, r#""$GIT" clone -q -s src/.git dst"#]);
}

#[test]
fn the_template_is_resolved_and_its_config_adopted() {
    compare(
        "template",
        &[
            r#""$GIT" clone -q --template=tmpl src dst"#,
            r#"GIT_TEMPLATE_DIR="$ROOT/tmpl" "$GIT" clone -q src dst"#,
            r#""$GIT" -c init.templateDir="$ROOT/tmpl" clone -q --bare src dst"#,
        ],
    );
}

#[test]
fn the_configured_remote_name_is_validated() {
    compare(
        "remote",
        &[
            r#""$GIT" -c clone.defaultRemoteName=bad~x clone src dst"#,
            r#""$GIT" -c clone.defaultRemoteName= clone -q src dst"#,
            r#""$GIT" -c clone.defaultRemoteName clone src dst"#,
            r#""$GIT" -c clone.defaultRemoteName=bad~x clone -q -o up src dst"#,
        ],
    );
}
