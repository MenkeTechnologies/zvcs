//! `git submodule` reads its `submodule.*` config in exactly two of the ported
//! subcommands, and both are exercised here against system `git` (2.55.0) in an
//! isolated, byte-identical environment:
//!
//!   * **`status`** consults whether each submodule is *active* — the union of
//!     the `submodule.active` pathspec and the per-submodule
//!     `submodule.<name>.active` boolean (git's `is_submodule_active`). An
//!     inactive submodule prints the `-<oid> <path>` line with no rev-name;
//!     an active one prints its state char and ` (<rev-name>)` suffix.
//!
//!   * **`init`** registers `submodule.<name>.active`, `submodule.<name>.url`
//!     and (only when the config has none) copies `submodule.<name>.update` out
//!     of `.gitmodules` into the repository config, and — with no pathspec and
//!     `submodule.active` set — restricts the walk to the active submodules.
//!     A value already present in the config is never overwritten by the
//!     `.gitmodules` copy (git's `git_config_get_string` precedence).
//!
//! Every other `submodule.*` key documented by `git help --config`
//! (`submodule.recurse`, `submodule.fetchJobs`, `submodule.<name>.branch`,
//! `submodule.<name>.fetchRecurseSubmodules`, `submodule.<name>.ignore`,
//! `submodule.alternate*`, `submodule.propagateBranches`,
//! `submodule.<name>.gitdir`) is consumed only by subcommands that this port
//! does not implement — `update`, `sync`, `set-branch`, `deinit`,
//! `absorbgitdirs` all bail — or by superproject commands outside `git
//! submodule` entirely (`fetch`/`pull`/`checkout`/`status`/`diff`). None of
//! them has any consuming behavior in `status`/`init`/`summary`/`foreach`, so
//! there is nothing further to port; this file pins the keys that are read.
//!
//! Each case builds a two-commit submodule origin and a superproject that
//! records it, then runs the same subcommand under both the port and system
//! `git`, comparing stdout, stderr, exit code and (for `init`) the resulting
//! `submodule.<name>.*` config the run wrote.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_git");

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A command carrying the deterministic, isolated environment shared by the
/// fixture builder and the run under test, so `git` and the port are directly
/// comparable. Fixed author/committer identities and dates keep every object id
/// reproducible across the separate fixtures the port and git each build.
fn env_cmd(bin: &str, repo: &Path, home: &Path) -> Command {
    let mut c = Command::new(bin);
    c.current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("ZVCS_HOME", home)
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@e")
        .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13 +0000")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@e")
        .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000");
    c
}

/// Run a system-`git` command in the fixture, asserting success. Used only to
/// build the fixture and to read/write config, never as the behavior under test.
fn git(repo: &Path, home: &Path, args: &[&str]) {
    let out = env_cmd("git", repo, home).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `git config --get <key>` in the fixture, `None` when unset.
fn read_cfg(repo: &Path, home: &Path, key: &str) -> Option<String> {
    let out = env_cmd("git", repo, home)
        .args(["config", "--get", key])
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// A fresh fixture: an isolated `HOME`, a two-commit submodule origin, and a
/// superproject that records that origin at `sm` with an `update = rebase`
/// strategy in `.gitmodules`. Returns the superproject, its home, and the
/// absolute origin path git stored as the submodule url.
fn fixture(tag: &str) -> (PathBuf, PathBuf, String) {
    let uniq = SEQ.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "zvcs-submodulecfg-{tag}-{}-{uniq}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // The submodule origin: two commits so its recorded tip is a real history.
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    git(&sub, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(sub.join("f"), "one\n").unwrap();
    git(&sub, &home, &["add", "f"]);
    git(&sub, &home, &["commit", "-q", "-m", "c1"]);
    std::fs::write(sub.join("f"), "two\n").unwrap();
    git(&sub, &home, &["add", "f"]);
    git(&sub, &home, &["commit", "-q", "-m", "c2"]);
    let sub_url = sub.to_str().unwrap().to_string();

    // The superproject that records it. `submodule add` over a `file://`-style
    // local path needs `protocol.file.allow=always` on git 2.55.0.
    let superr = root.join("super");
    std::fs::create_dir_all(&superr).unwrap();
    git(&superr, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(superr.join("t"), "top\n").unwrap();
    git(&superr, &home, &["add", "t"]);
    git(&superr, &home, &["commit", "-q", "-m", "init"]);
    git(
        &superr,
        &home,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &sub_url,
            "sm",
        ],
    );
    // Record an update strategy in .gitmodules so init's copy path is exercised.
    git(
        &superr,
        &home,
        &["config", "-f", ".gitmodules", "submodule.sm.update", "rebase"],
    );
    git(&superr, &home, &["add", ".gitmodules"]);
    git(&superr, &home, &["commit", "-q", "-m", "addsub"]);

    (superr, home, sub_url)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The three `submodule.sm.*` config values a run may have written, read back
/// with system git so the state reflects whatever the binary under test wrote.
#[derive(PartialEq, Debug)]
struct SmConfig {
    url: Option<String>,
    active: Option<String>,
    update: Option<String>,
}

fn read_sm_config(repo: &Path, home: &Path) -> SmConfig {
    SmConfig {
        url: read_cfg(repo, home, "submodule.sm.url"),
        active: read_cfg(repo, home, "submodule.sm.active"),
        update: read_cfg(repo, home, "submodule.sm.update"),
    }
}

/// The result of a subcommand run plus the `submodule.sm.*` config it left.
struct Res {
    out: Output,
    cfg: SmConfig,
    repo: PathBuf,
    /// The fixture's own submodule origin path, embedded in init's message and
    /// in the registered url; normalized away before cross-fixture comparison.
    url: String,
}

impl Drop for Res {
    fn drop(&mut self) {
        // Each fixture's root is the parent of `super/`; clean the whole tree.
        if let Some(root) = self.repo.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

/// Build a fresh fixture, optionally drop the `submodule.sm` config section
/// (to simulate an unregistered submodule for `init`), apply `config`, then run
/// `<bin> submodule <args>` and read back the resulting `submodule.sm.*` config.
fn scenario(
    bin: &str,
    tag: &str,
    deregister: bool,
    config: &[(&str, &str)],
    args: &[&str],
) -> Res {
    let (repo, home, url) = fixture(tag);
    if deregister {
        // `submodule add` registers submodule.sm.url/.active; clear them so
        // init has to re-register from .gitmodules.
        git(&repo, &home, &["config", "--remove-section", "submodule.sm"]);
    }
    for (k, v) in config {
        git(&repo, &home, &["config", k, v]);
    }
    let mut full = vec!["submodule"];
    full.extend_from_slice(args);
    let out = env_cmd(bin, &repo, &home).args(&full).output().unwrap();
    let cfg = read_sm_config(&repo, &home);
    Res { out, cfg, repo, url }
}

/// Assert the port and system git agree on every process observable, with each
/// fixture's own submodule origin path folded to a placeholder so runs from
/// different temp directories are comparable.
fn assert_output_matches(z: &Res, g: &Res, what: &str) {
    assert_eq!(z.out.status.code(), g.out.status.code(), "{what}: exit code");
    assert_eq!(
        stdout(&z.out).replace(&z.url, "<URL>"),
        stdout(&g.out).replace(&g.url, "<URL>"),
        "{what}: stdout"
    );
    assert_eq!(
        stderr(&z.out).replace(&z.url, "<URL>"),
        stderr(&g.out).replace(&g.url, "<URL>"),
        "{what}: stderr"
    );
}

// -------------------------------------------------------------- status ------

#[test]
fn status_default_reports_active_submodule_with_rev_name() {
    // A registered, populated submodule whose HEAD matches the superproject
    // index prints the space state and a `(<rev-name>)` suffix. The port and
    // git must agree byte-for-byte, and the line must not be the `-` form.
    let z = scenario(BIN, "st-def", false, &[], &["status"]);
    let g = scenario("git", "st-def-g", false, &[], &["status"]);
    let so = stdout(&z.out);
    assert!(
        so.contains(" sm (") && !so.starts_with('-'),
        "active submodule must carry a rev-name and no `-` state: {so:?}"
    );
    assert_output_matches(&z, &g, "status default");
}

#[test]
fn status_submodule_active_pathspec_marks_inactive() {
    // `submodule.active` set to a pathspec that does not match `sm` makes the
    // submodule inactive: git prints `-<oid> sm` with no rev-name. This is the
    // `submodule.active` read in status (via is_submodule_active).
    // Deregister first so no per-name `submodule.sm.active` (which `submodule
    // add` writes as `true`) overrides the pathspec.
    let cfg = &[("submodule.active", "does/not/match")];
    let z = scenario(BIN, "st-act", true, cfg, &["status"]);
    let g = scenario("git", "st-act-g", true, cfg, &["status"]);
    let so = stdout(&z.out);
    assert!(
        so.starts_with('-') && !so.contains(" ("),
        "submodule.active pathspec miss must print `-` with no rev-name: {so:?}"
    );
    assert_output_matches(&z, &g, "status submodule.active pathspec");
}

#[test]
fn status_per_name_active_false_marks_inactive() {
    // `submodule.sm.active=false` overrides to inactive even without a
    // `submodule.active` pathspec — the per-name half of is_submodule_active.
    let cfg = &[("submodule.sm.active", "false")];
    let z = scenario(BIN, "st-nact", false, cfg, &["status"]);
    let g = scenario("git", "st-nact-g", false, cfg, &["status"]);
    let so = stdout(&z.out);
    assert!(
        so.starts_with('-'),
        "submodule.sm.active=false must print `-`: {so:?}"
    );
    assert_output_matches(&z, &g, "status submodule.<name>.active=false");
}

// ---------------------------------------------------------------- init ------

#[test]
fn init_registers_url_active_and_update_from_gitmodules() {
    // From an unregistered submodule, init copies the url out of .gitmodules,
    // marks it active, and copies the `update = rebase` strategy. All three
    // keys, the printed message and the exit code must match git.
    let z = scenario(BIN, "in-reg", true, &[], &["init"]);
    let g = scenario("git", "in-reg-g", true, &[], &["init"]);
    assert_eq!(z.cfg.active.as_deref(), Some("true"), "init must set active=true");
    assert_eq!(
        z.cfg.update.as_deref(),
        Some("rebase"),
        "init must copy update=rebase from .gitmodules"
    );
    assert!(
        z.cfg.url.as_deref().is_some_and(|u| u.ends_with("/sub")),
        "registered url must be the submodule origin path: {:?}",
        z.cfg.url
    );
    // The url embeds each fixture's own temp path, so compare it by suffix; the
    // two config-only keys must match git exactly.
    assert!(
        g.cfg.url.as_deref().is_some_and(|u| u.ends_with("/sub")),
        "git registered url must be the submodule origin path: {:?}",
        g.cfg.url
    );
    assert_eq!(z.cfg.active, g.cfg.active, "active must match git");
    assert_eq!(z.cfg.update, g.cfg.update, "update must match git");
    assert_output_matches(&z, &g, "init registers url/active/update");
}

#[test]
fn init_does_not_overwrite_existing_config_values() {
    // A url and update already in the config are git's `git_config_get_string`
    // precedence: init leaves both untouched, and never copies the .gitmodules
    // `update = rebase` over the existing `merge`.
    let cfg = &[
        ("submodule.sm.url", "https://example.invalid/pre"),
        ("submodule.sm.update", "merge"),
    ];
    let z = scenario(BIN, "in-prec", true, cfg, &["init"]);
    let g = scenario("git", "in-prec-g", true, cfg, &["init"]);
    assert_eq!(
        z.cfg.url.as_deref(),
        Some("https://example.invalid/pre"),
        "existing url must be preserved"
    );
    assert_eq!(
        z.cfg.update.as_deref(),
        Some("merge"),
        "existing update must not be overwritten by .gitmodules"
    );
    assert_eq!(z.cfg, g.cfg, "resulting submodule.sm.* config must match git");
    assert_output_matches(&z, &g, "init config precedence");
}

#[test]
fn init_with_active_pathspec_skips_inactive_submodule() {
    // No pathspec and `submodule.active` set restricts init to active
    // submodules (git's module_list_active): a non-matching pathspec means `sm`
    // is skipped and nothing is registered.
    let cfg = &[("submodule.active", "does/not/match")];
    let z = scenario(BIN, "in-act", true, cfg, &["init"]);
    let g = scenario("git", "in-act-g", true, cfg, &["init"]);
    assert_eq!(
        z.cfg,
        SmConfig {
            url: None,
            active: None,
            update: None,
        },
        "inactive submodule must not be registered"
    );
    assert_eq!(z.cfg, g.cfg, "resulting submodule.sm.* config must match git");
    assert_output_matches(&z, &g, "init submodule.active filter");
}
