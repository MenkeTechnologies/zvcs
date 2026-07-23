//! `git init` resolves the initial branch with git's exact precedence:
//! `-b`/`--initial-branch` on the command line, else the `init.defaultBranch`
//! config value, else the compiled-in default `master`. Each case asserts the
//! resulting `.git/HEAD` byte-for-byte and cross-checks it against stock git run
//! under an identical HOME/env, guarding against the gix `main` fallback leaking
//! through and against the config default being ignored.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// Create an isolated root with a `home/` (optionally carrying a `.gitconfig`
/// with `init.defaultBranch = <branch>`) plus empty `zvcs/` and `real/` dirs to
/// init into.
fn fixture(tag: &str, default_branch: Option<&str>) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-initcfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();

    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    if let Some(branch) = default_branch {
        std::fs::write(
            home.join(".gitconfig"),
            format!("[init]\n\tdefaultBranch = {branch}\n"),
        )
        .unwrap();
    }

    let zvcs = root.join("zvcs");
    let real = root.join("real");
    std::fs::create_dir_all(&zvcs).unwrap();
    std::fs::create_dir_all(&real).unwrap();
    (home, zvcs, real)
}

/// Run `init` (with any extra flags) under the given HOME + `GIT_CONFIG_NOSYSTEM`
/// and return the resulting `.git/HEAD` contents verbatim.
fn init_head(bin: &str, dir: &Path, home: &Path, extra: &[&str]) -> String {
    let mut args = vec!["init", "-q"];
    args.extend_from_slice(extra);
    let ok = Command::new(bin)
        .args(&args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success();
    assert!(ok, "{bin} {args:?} failed in {dir:?}");
    std::fs::read_to_string(dir.join(".git/HEAD")).unwrap()
}

/// Assert zvcs and stock git produce the same `.git/HEAD`, and that it names the
/// expected branch.
fn assert_head(tag: &str, default_branch: Option<&str>, extra: &[&str], want_branch: &str) {
    let (home, zvcs, real) = fixture(tag, default_branch);
    let zvcs_head = init_head(BIN, &zvcs, &home, extra);
    let real_head = init_head("git", &real, &home, extra);
    let expected = format!("ref: refs/heads/{want_branch}\n");
    assert_eq!(zvcs_head, real_head, "zvcs HEAD differs from stock git ({tag})");
    assert_eq!(zvcs_head, expected, "zvcs HEAD not the expected branch ({tag})");
    let _ = std::fs::remove_dir_all(zvcs.parent().unwrap());
}

#[test]
fn init_default_branch_config_sets_head() {
    // init.defaultBranch, no CLI flag -> HEAD points at the configured branch.
    assert_head("cfg", Some("trunk"), &[], "trunk");
}

#[test]
fn init_b_flag_overrides_default_branch_config() {
    // -b <name> wins over init.defaultBranch.
    assert_head("bflag", Some("trunk"), &["-b", "feature"], "feature");
}

#[test]
fn init_initial_branch_eq_overrides_default_branch_config() {
    // --initial-branch=<name> wins over init.defaultBranch.
    assert_head("eqflag", Some("trunk"), &["--initial-branch=dev"], "dev");
}

#[test]
fn init_without_config_or_flag_defaults_to_master() {
    // No init.defaultBranch and no flag -> git's compiled default `master`
    // (must NOT leak gix's `main` fallback).
    assert_head("nocfg", None, &[], "master");
}

#[test]
fn init_no_config_flag_still_wins() {
    // With no config, an explicit -b is still honored verbatim.
    assert_head("nocfg-bflag", None, &["-b", "release"], "release");
}

/// Run `init` (with extra flags) in `dir` under the isolated env, asserting it
/// succeeds. Unlike `init_head` this does not read HEAD, so it works for flags
/// (`--separate-git-dir`, `--bare`) whose git dir is not `<dir>/.git`.
fn run_init(bin: &str, dir: &Path, home: &Path, extra: &[&str]) {
    let mut args = vec!["init", "-q"];
    args.extend_from_slice(extra);
    let ok = Command::new(bin)
        .args(&args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success();
    assert!(ok, "{bin} {args:?} failed in {dir:?}");
}

/// Read a single config value from the repo at/inside `dir` using stock git
/// (ground truth for both zvcs- and git-created repos). `None` when unset.
fn git_config_get(dir: &Path, key: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "--get", key])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// `--shared=<perm>` must write the same `core.sharedrepository` and
/// `receive.denyNonFastforwards` values stock git does, using git's
/// compatibility encoding (`group`->`1`, `world`->`2`, `0xxx` kept as octal).
fn assert_shared_config(tag: &str, perm: &str, want_shared: &str) {
    let (home, zvcs, real) = fixture(tag, None);
    let flag = format!("--shared={perm}");
    run_init(BIN, &zvcs, &home, &[&flag]);
    run_init("git", &real, &home, &[&flag]);

    let z_shared = git_config_get(&zvcs, "core.sharedrepository");
    let g_shared = git_config_get(&real, "core.sharedrepository");
    assert_eq!(z_shared, g_shared, "core.sharedrepository differs ({tag})");
    assert_eq!(
        z_shared.as_deref(),
        Some(want_shared),
        "core.sharedrepository value ({tag})"
    );

    let z_deny = git_config_get(&zvcs, "receive.denyNonFastforwards");
    let g_deny = git_config_get(&real, "receive.denyNonFastforwards");
    assert_eq!(z_deny, g_deny, "receive.denyNonFastforwards differs ({tag})");
    assert_eq!(z_deny.as_deref(), Some("true"), "denyNonFastforwards set ({tag})");

    let _ = std::fs::remove_dir_all(zvcs.parent().unwrap());
}

#[test]
fn init_shared_group_writes_compat_config() {
    assert_shared_config("shared-group", "group", "1");
}

#[test]
fn init_shared_everybody_writes_compat_config() {
    assert_shared_config("shared-all", "all", "2");
}

#[test]
fn init_shared_octal_writes_filemode() {
    assert_shared_config("shared-octal", "0640", "0640");
}

#[test]
fn init_shared_dir_perms_match_stock_git() {
    // The permission widening (calc_shared_perm/adjust_shared_perm port) must
    // produce the same modes stock git does. Compared zvcs-vs-git under one
    // shared umask, so it is robust to whatever the CI umask happens to be.
    use std::os::unix::fs::PermissionsExt;

    let (home, zvcs, real) = fixture("shared-perm", None);
    run_init(BIN, &zvcs, &home, &["--shared=group"]);
    run_init("git", &real, &home, &["--shared=group"]);

    for rel in ["", "objects", "refs", "config"] {
        let z = zvcs.join(".git").join(rel);
        let g = real.join(".git").join(rel);
        let zm = std::fs::symlink_metadata(&z).unwrap().permissions().mode() & 0o7777;
        let gm = std::fs::symlink_metadata(&g).unwrap().permissions().mode() & 0o7777;
        assert_eq!(zm, gm, "mode of .git/{rel} differs: zvcs {zm:o} vs git {gm:o}");
    }
    let _ = std::fs::remove_dir_all(zvcs.parent().unwrap());
}

#[test]
fn init_separate_git_dir_writes_gitfile_and_relocates() {
    // `--separate-git-dir=<dir>` puts the real git dir at <dir> and leaves a
    // `gitdir: <abs>` link file at <target>/.git, matching git's format.
    let (home, zvcs, real) = fixture("sepgd", None);
    let z_gd = zvcs.join("gitdir-store");
    let g_gd = real.join("gitdir-store");
    run_init(BIN, &zvcs, &home, &[&format!("--separate-git-dir={}", z_gd.display())]);
    run_init("git", &real, &home, &[&format!("--separate-git-dir={}", g_gd.display())]);

    // The worktree's .git is a file, not a dir, for both.
    assert!(zvcs.join(".git").is_file(), "zvcs .git should be a link file");
    assert!(real.join(".git").is_file(), "git .git should be a link file");

    // Link-file shape matches git: "gitdir: <abs>\n" pointing at the real dir.
    let z_link = std::fs::read_to_string(zvcs.join(".git")).unwrap();
    let z_real = z_gd.canonicalize().unwrap();
    assert_eq!(z_link, format!("gitdir: {}\n", z_real.display()));

    // The relocated dir holds the repo; HEAD matches stock git's relocated HEAD.
    let z_head = std::fs::read_to_string(z_real.join("HEAD")).unwrap();
    let g_head = std::fs::read_to_string(g_gd.canonicalize().unwrap().join("HEAD")).unwrap();
    assert_eq!(z_head, g_head, "relocated HEAD differs from stock git");

    let _ = std::fs::remove_dir_all(zvcs.parent().unwrap());
}

#[test]
fn init_separate_git_dir_conflicts_with_bare() {
    // git refuses --separate-git-dir together with --bare.
    let (home, zvcs, _real) = fixture("sepgd-bare", None);
    let ok = Command::new(BIN)
        .args(["init", "-q", "--bare", "--separate-git-dir=/tmp/whatever"])
        .current_dir(&zvcs)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success();
    assert!(!ok, "--separate-git-dir with --bare must fail");
    let _ = std::fs::remove_dir_all(zvcs.parent().unwrap());
}

#[test]
fn init_template_replaces_default_template() {
    // `--template=<dir>` seeds ONLY the given template's files (a custom hook +
    // description), with no default sample hooks and no info/ — matching git,
    // which uses the template dir instead of the built-in default.
    let (home, zvcs, real) = fixture("tpl", None);

    // A minimal custom template: one hook, a description, nothing else.
    let tpl = zvcs.parent().unwrap().join("tpl-src");
    std::fs::create_dir_all(tpl.join("hooks")).unwrap();
    std::fs::write(tpl.join("hooks/pre-commit"), "#!/bin/sh\necho custom\n").unwrap();
    std::fs::write(tpl.join("description"), "CUSTOM DESC\n").unwrap();

    let flag = format!("--template={}", tpl.display());
    run_init(BIN, &zvcs, &home, &[&flag]);
    run_init("git", &real, &home, &[&flag]);

    let zg = zvcs.join(".git");
    let gg = real.join(".git");

    // Custom files present and identical to git.
    assert_eq!(
        std::fs::read_to_string(zg.join("description")).unwrap(),
        std::fs::read_to_string(gg.join("description")).unwrap()
    );
    assert!(zg.join("hooks/pre-commit").is_file(), "custom hook missing");

    // Exactly the template's hooks — no default *.sample hooks leaked in.
    let mut z_hooks: Vec<String> = std::fs::read_dir(zg.join("hooks"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    z_hooks.sort();
    assert_eq!(z_hooks, vec!["pre-commit".to_string()], "unexpected hooks");

    // The template omitted info/, so (like git) there is no top-level info/.
    assert!(!zg.join("info").exists(), "info/ should be absent (git parity)");
    assert!(!gg.join("info").exists(), "sanity: stock git also omits info/");

    let _ = std::fs::remove_dir_all(zvcs.parent().unwrap());
}
