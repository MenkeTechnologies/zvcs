//! `git submodule` against a checkout whose `.git` file leads nowhere, because
//! `.git/modules/<name>` was removed, and `deinit` of a submodule that was never
//! cloned.
//!
//! * status_submodule() reads `<path>/.git` with read_gitfile(), and the foreach
//!   and sync callbacks ask is_submodule_populated_gently(path, NULL)
//!   (builtin/submodule--helper.c:774-776, :350, :1481): with no error-code
//!   pointer read_gitfile_gently() dies `gitfile does not point to a valid
//!   repository: <path>/.git` (setup.c:925-951). zvcs printed `-<oid> suba` or
//!   skipped the submodule and exited 0.
//! * `git rm -n` of such a gitlink reaches the same die through
//!   bad_to_remove_submodule() -> submodule_uses_gitfile() (submodule.c:2003-2017),
//!   so `deinit` without `-f`, whose `rm -qn` child fails, adds its own
//!   "contains local modifications" die.
//! * `update` opens the checkout in ensure_core_worktree() and dies `could not get
//!   a repository handle for submodule '<path>'` (submodule--helper.c:2722-2729).
//! * absorbgitdirs repoints the `.git` file and recurses through a child whose
//!   setup dies on that `.git` before the parent's `could not recurse`.
//! * submodule_unset_core_worktree() goes through repo_config_set_in_file_gently(),
//!   whose lock on a missing `modules/<name>/config` fails `error: could not lock
//!   config file ...: No such file or directory` (config.c:3066-3072) ahead of the
//!   warning, and the warning names `sub->path`, not the display path.
//!
//! Each case builds the fixture with stock git at the same path for both
//! binaries, so the absolute paths in the output agree. Skipped when no stock git
//! is available.

#[path = "support/stock_git.rs"]
mod stock_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(bin: &str, dir: &Path, home: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(bin);
    for (k, _) in std::env::vars() {
        if k.starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    c.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "protocol.file.allow")
        .env("GIT_CONFIG_VALUE_0", "always")
        .env("GIT_CONFIG_KEY_1", "init.defaultBranch")
        .env("GIT_CONFIG_VALUE_1", "main")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env("LC_ALL", "C")
        .output()
        .expect("spawn git")
}

fn ok(bin: &str, dir: &Path, home: &Path, args: &[&str]) {
    let out = run(bin, dir, home, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `up_a`, `up_b` and `super` with `suba` and `nest/b` registered; `cloned`
/// decides whether `super`'s submodules are checked out.
fn fixture(stock: &str, root: &Path, cloned: bool) -> PathBuf {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    for up in ["up_a", "up_b"] {
        ok(stock, &root, &home, &["init", "-q", up]);
        let dir = root.join(up);
        std::fs::write(dir.join("f"), up).unwrap();
        ok(stock, &dir, &home, &["add", "f"]);
        ok(stock, &dir, &home, &["commit", "-q", "-m", up]);
    }
    let up = root.join("up_super");
    ok(stock, &root, &home, &["init", "-q", "up_super"]);
    let url_a = format!("file://{}", root.join("up_a").display());
    let url_b = format!("file://{}", root.join("up_b").display());
    ok(stock, &up, &home, &["submodule", "add", "-q", &url_a, "suba"]);
    ok(stock, &up, &home, &["submodule", "add", "-q", &url_b, "nest/b"]);
    ok(stock, &up, &home, &["commit", "-q", "-m", "subs"]);
    ok(stock, &root, &home, &["clone", "-q", up.to_str().unwrap(), "super"]);
    let sup = root.join("super");
    if cloned {
        ok(stock, &sup, &home, &["submodule", "update", "-q", "--init"]);
    }
    sup
}

/// Run `args` in `super/<sub>` with each binary on its own fresh fixture at the
/// same path, after `prepare` reshaped it; returns `(stock, zvcs)`.
fn twin(
    stock: &str,
    tag: &str,
    cloned: bool,
    prepare: fn(&Path),
    sub: &str,
    args: &[&str],
) -> (Output, Output) {
    let root = std::env::temp_dir().join(format!("zvcs-sm-broken-{tag}-{}", std::process::id()));
    let mut outs = Vec::new();
    for bin in [stock, BIN] {
        let sup = fixture(stock, &root, cloned);
        prepare(&sup);
        let home = sup.parent().unwrap().join("home");
        outs.push(run(bin, &sup.join(sub), &home, args));
    }
    let _ = std::fs::remove_dir_all(&root);
    let z = outs.pop().unwrap();
    (outs.pop().unwrap(), z)
}

fn assert_same(what: &str, s: &Output, z: &Output) {
    assert_eq!(
        String::from_utf8_lossy(&z.stderr),
        String::from_utf8_lossy(&s.stderr),
        "{what}: stderr"
    );
    assert_eq!(
        String::from_utf8_lossy(&z.stdout),
        String::from_utf8_lossy(&s.stdout),
        "{what}: stdout"
    );
    assert_eq!(z.status.code(), s.status.code(), "{what}: exit code");
}

fn drop_modules_dir(sup: &Path) {
    std::fs::remove_dir_all(sup.join(".git/modules/suba")).unwrap();
}

fn nothing(_: &Path) {}

#[test]
fn a_gitfile_leading_nowhere_is_fatal_where_git_reads_it_without_an_error_pointer() {
    let Some(stock) = stock_git::stock_git() else { return };
    let cases: &[&[&str]] = &[
        &["submodule", "status"],
        &["submodule", "status", "--cached"],
        &["submodule", "status", "nest"],
        &["submodule", "foreach", "echo $sm_path"],
        &["submodule", "sync"],
        &["rm", "-qn", "suba"],
        &["submodule", "deinit", "suba"],
        &["submodule", "update"],
        &["submodule", "absorbgitdirs"],
    ];
    for (i, args) in cases.iter().enumerate() {
        let (s, z) = twin(stock, &format!("nowhere{i}"), true, drop_modules_dir, "", args);
        assert_same(&format!("{args:?}"), &s, &z);
    }
}

#[test]
fn deinit_of_an_uncloned_submodule_reports_the_config_lock_and_names_the_path() {
    let Some(stock) = stock_git::stock_git() else { return };
    let cases: &[(&str, &[&str])] = &[
        ("", &["submodule", "deinit", "suba"]),
        ("", &["submodule", "deinit", "-f", "--all"]),
        ("nest", &["submodule", "deinit", "-f", "b"]),
    ];
    for (i, (sub, args)) in cases.iter().enumerate() {
        let (s, z) = twin(stock, &format!("uncloned{i}"), false, nothing, sub, args);
        assert_same(&format!("{sub}: {args:?}"), &s, &z);
    }
}
