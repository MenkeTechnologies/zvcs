//! The native plugin system end to end: `git znative` installs a cdylib into
//! the global store, records the verbs it registers, and dispatch then serves
//! `git <verb>` out of the plugin — resolving it before the `git-<verb>` PATH
//! lookup, and letting an override delegate back to the original verb.
//!
//! The example plugin (`examples/plugin-hello`) is the fixture: it is the same
//! artifact a third party would ship, built through the same ABI crate, so a
//! break in the host/plugin contract fails here rather than in someone's repo.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

/// The repository root — `src/extensions/` is the package directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn git(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home.join("zvcs"))
        .output()
        .unwrap()
}

fn ok(out: &Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A hermetic repo with its own HOME and `$ZVCS_HOME`, so the plugin store
/// under test is never the developer's real one.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-znative-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();

    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&git(&repo, &home, &["init", "-q", "-b", "main"]), "init");
    ok(
        &git(
            &repo,
            &home,
            &["-c", "user.email=t@e.x", "-c", "user.name=t", "commit", "--allow-empty", "-q", "-m", "first"],
        ),
        "commit",
    );
    (repo, home)
}

/// Build a native example plugin and lay its artifact out the way a plugin that
/// publishes binaries ships: the cdylib plus its `znative.toml`. Returns the
/// directory to install from. `None` when there is no cargo to build with,
/// which is the one environment this test cannot run in.
fn staged_plugin(root: &Path, work: &Path, example: &str) -> Option<PathBuf> {
    let manifest = root.join("examples").join(example).join("Cargo.toml");
    let target = work.join(format!("build-{example}"));
    let out = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target)
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "building the example plugin failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rel = target.join("release");
    let lib = std::fs::read_dir(&rel)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| {
            n.starts_with(std::env::consts::DLL_PREFIX) && n.ends_with(std::env::consts::DLL_SUFFIX)
        })
        .expect("the example plugin produced no cdylib");

    let staged = work.join(format!("staged-{example}"));
    std::fs::create_dir_all(&staged).unwrap();
    std::fs::copy(rel.join(&lib), staged.join(&lib)).unwrap();
    std::fs::copy(root.join("examples").join(example).join("znative.toml"), staged.join("znative.toml"))
        .unwrap();
    Some(staged)
}

#[test]
fn installs_a_native_plugin_and_serves_its_verb() {
    let (repo, home) = fixture("install");
    let root = repo_root();
    let Some(staged) = staged_plugin(&root, &home, "plugin-hello") else {
        eprintln!("no cargo on PATH; skipping");
        return;
    };

    // An empty store says so, and the verb is unknown before the install.
    let out = git(&repo, &home, &["znative", "list"]);
    assert_eq!(ok(&out, "znative list").trim(), "znative: no plugins installed");
    let out = git(&repo, &home, &["hello"]);
    assert!(!out.status.success(), "`git hello` resolved before anything was installed");

    // Install from the staged directory: a local path never touches the network.
    let spec = format!("path:{}", staged.display());
    let out = ok(&git(&repo, &home, &["znative", "add", &spec]), "znative add");
    assert!(out.contains("added hello@0.1.0 (native)"), "{out}");
    // The verbs it registered are discovered by loading it, not declared.
    assert!(out.contains("verbs: hello version (override)"), "{out}");

    // The index and its derived tables now name the plugin.
    let out = ok(&git(&repo, &home, &["znative", "list"]), "znative list");
    assert!(out.starts_with("hello"), "{out}");
    let out = ok(&git(&repo, &home, &["znative", "info", "hello"]), "znative info");
    assert!(out.contains("kind       native"), "{out}");
    assert!(out.contains("verbs      hello"), "{out}");
    assert!(out.contains("overrides  version"), "{out}");
    assert!(out.contains("integrity  sha256-"), "{out}");

    // The added verb now dispatches into the plugin, which reads the repository
    // back through the host API.
    let out = ok(&git(&repo, &home, &["hello"]), "git hello");
    assert!(out.contains("hello from a native plugin"), "{out}");
    assert!(out.contains("branch   main"), "{out}");
    assert!(out.lines().any(|l| l.starts_with("  head     ") && l.trim().len() > 12), "{out}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn an_override_runs_before_the_original_and_can_delegate_to_it() {
    let (repo, home) = fixture("override");
    let root = repo_root();
    let Some(staged) = staged_plugin(&root, &home, "plugin-hello") else {
        eprintln!("no cargo on PATH; skipping");
        return;
    };

    // `git version` before the install is the plain built-in answer.
    let before = ok(&git(&repo, &home, &["version"]), "git version");
    assert!(before.starts_with("git version"), "{before}");

    ok(&git(&repo, &home, &["znative", "add", &format!("path:{}", staged.display())]), "add");

    // After it, the plugin's line comes first and the ORIGINAL verb still runs —
    // the override calls `dispatch_verb`, which must not re-enter the override.
    let after = ok(&git(&repo, &home, &["version"]), "git version (overridden)");
    let mut lines = after.lines();
    assert_eq!(lines.next(), Some("plugin `hello` is loaded"));
    assert_eq!(lines.next(), Some(before.trim()));

    // Removing the plugin restores the built-in verb and empties the store.
    ok(&git(&repo, &home, &["znative", "remove", "hello"]), "znative remove");
    let restored = ok(&git(&repo, &home, &["version"]), "git version (restored)");
    assert_eq!(restored, before);
    assert!(!home.join("zvcs/pkg/overrides.tsv").exists(), "the override table outlived the plugin");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_plugin_verb_wins_over_a_same_named_dashed_external() {
    // Plugins resolve after builtins and before PATH — the slot zsh gives an
    // autoloaded module builtin. A `git-hello` on PATH must not shadow one.
    let (repo, home) = fixture("precedence");
    let root = repo_root();
    let Some(staged) = staged_plugin(&root, &home, "plugin-hello") else {
        eprintln!("no cargo on PATH; skipping");
        return;
    };

    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let external = bin.join("git-hello");
    std::fs::write(&external, b"#!/bin/sh\necho external\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    // With nothing installed, the external is what answers.
    let out = Command::new(BIN)
        .arg("hello")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("PATH", &path)
        .env("ZVCS_HOME", home.join("zvcs"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "external");

    ok(&git(&repo, &home, &["znative", "add", &format!("path:{}", staged.display())]), "add");

    // With the plugin installed, it wins.
    let out = Command::new(BIN)
        .arg("hello")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("PATH", &path)
        .env("ZVCS_HOME", home.join("zvcs"))
        .output()
        .unwrap();
    let stdout = ok(&out, "git hello with an external on PATH");
    assert!(stdout.contains("hello from a native plugin"), "{stdout}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_script_plugin_serves_its_verb_from_the_store() {
    // The second plugin kind: a repo of `git-<verb>` executables, which is the
    // shape every third-party git subcommand already ships in.
    let (repo, home) = fixture("script");
    let src = home.join("script-plugin");
    std::fs::create_dir_all(src.join("bin")).unwrap();
    std::fs::write(
        src.join("bin/git-greet"),
        b"#!/bin/sh\necho \"greetings from a script plugin: $*\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            src.join("bin/git-greet"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    std::fs::write(
        src.join("znative.toml"),
        b"[plugin]\nname = \"greeter\"\nversion = \"2.0.0\"\n\n[script]\nbin = [\"bin\"]\n",
    )
    .unwrap();

    let out = ok(
        &git(&repo, &home, &["znative", "add", &format!("path:{}", src.display())]),
        "znative add (script)",
    );
    assert!(out.contains("added greeter@2.0.0 (script)"), "{out}");
    assert!(out.contains("verbs: greet"), "{out}");

    // The executable runs out of the STORE copy, not the source directory.
    let out = ok(&git(&repo, &home, &["greet", "world"]), "git greet");
    assert_eq!(out.trim(), "greetings from a script plugin: world");
    let _ = std::fs::remove_dir_all(&src);
    let out = ok(&git(&repo, &home, &["greet", "again"]), "git greet after the source is gone");
    assert_eq!(out.trim(), "greetings from a script plugin: again");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_machine_with_no_plugins_keeps_the_verb_tables_absent() {
    // The whole per-process cost model: with nothing installed there is no table
    // to read, so every command's plugin check is one failed `stat`.
    let (repo, home) = fixture("cold");
    ok(&git(&repo, &home, &["status", "--porcelain"]), "status");
    let pkg = home.join("zvcs/pkg");
    assert!(!pkg.join("verbs.tsv").exists());
    assert!(!pkg.join("overrides.tsv").exists());
    assert!(!pkg.join("installed.toml").exists());
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn the_wip_example_composes_the_porcelain_through_the_host() {
    // `examples/plugin-wip` is the useful-work example: it runs `diff`, `add`
    // and `commit` through `host.run`, in-process, and reports their status.
    // Shipping it means the ABI's `run` path is covered by a real plugin.
    let (repo, home) = fixture("wip");
    let root = repo_root();
    let Some(staged) = staged_plugin(&root, &home, "plugin-wip") else {
        eprintln!("no cargo on PATH; skipping");
        return;
    };
    ok(&git(&repo, &home, &["znative", "add", &format!("path:{}", staged.display())]), "add");
    ok(&git(&repo, &home, &["config", "user.email", "t@e.x"]), "config email");
    ok(&git(&repo, &home, &["config", "user.name", "t"]), "config name");

    std::fs::write(repo.join("a.txt"), b"one\n").unwrap();
    ok(&git(&repo, &home, &["wip"]), "git wip");
    let log = ok(&git(&repo, &home, &["log", "--oneline", "-1"]), "log");
    assert!(log.contains("wip on main"), "{log}");

    // Its message argument wins over the generated one.
    std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
    ok(&git(&repo, &home, &["wip", "second", "pass"]), "git wip <msg>");
    let log = ok(&git(&repo, &home, &["log", "--oneline", "-1"]), "log");
    assert!(log.contains("second pass"), "{log}");

    // A clean tree is refused, non-zero — the status of `diff --quiet HEAD`
    // read back through the host, not a guess.
    let out = git(&repo, &home, &["wip"]);
    assert!(!out.status.success(), "wip committed an empty change");
    assert!(String::from_utf8_lossy(&out.stderr).contains("nothing to commit"));

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn the_todo_example_installs_and_runs_straight_from_its_source_tree() {
    // `examples/plugin-todo` is the script-kind example: no ABI, no build step,
    // installed from the repository as it stands.
    let (repo, home) = fixture("todo");
    let example = repo_root().join("examples/plugin-todo");

    let out = ok(
        &git(&repo, &home, &["znative", "add", &format!("path:{}", example.display())]),
        "znative add (todo)",
    );
    assert!(out.contains("added todo@0.1.0 (script)"), "{out}");
    assert!(out.contains("verbs: todo"), "{out}");

    std::fs::write(repo.join("a.rs"), b"fn main() {} // TODO: write it\nfine\n").unwrap();
    ok(&git(&repo, &home, &["add", "a.rs"]), "add");
    let out = ok(&git(&repo, &home, &["todo"]), "git todo");
    assert!(out.contains("a.rs:1:"), "{out}");
    assert!(!out.contains("fine"), "matched an untagged line: {out}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
