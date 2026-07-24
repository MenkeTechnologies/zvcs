//! `ztrigger` / `zwatch` — DIR-addressed hook arming.
//!
//! Isolated from the real machine: `ZVCS_HOME` redirects the index/socket and
//! `HOME`/`XDG_CONFIG_HOME` redirect `--global` writes to a throwaway config, so
//! the auto-flipped `zvcs.autohook` never touches the developer's `~/.gitconfig`.
//! `ZVCS_NO_DAEMON` keeps the verbs from spawning a daemon in CI; the trigger is
//! still fired synchronously via `ztrigger test`.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(["-c", "user.email=t@e.x", "-c", "user.name=t"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

/// Run the zvcs binary with the machine fully sandboxed (state dir, global
/// config, no daemon).
fn zvcs(root: &Path, cwd: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("ZVCS_HOME", root.join("home"))
        .env("HOME", root.join("cfg"))
        .env("XDG_CONFIG_HOME", root.join("cfg/.config"))
        .env("ZVCS_NO_DAEMON", "1")
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.success())
}

#[test]
fn ztrigger_arms_dir_and_fires() {
    let root = std::env::temp_dir().join(format!("zvcs-trig-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("cfg/.config")).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "c0"]);

    let marker = root.join("fired.txt");
    let repo_s = repo.to_str().unwrap();

    // Arm the repo BY PATH from an unrelated cwd (the point of ztrigger vs zhook).
    let (_o, ok) = zvcs(&root, &root, &[
        "ztrigger", repo_s, "echo", "$ZVCS_EVENT", ">", marker.to_str().unwrap(),
    ]);
    assert!(ok, "ztrigger set failed");

    // Effect 1: the repo's LOCAL zvcs.hook was written.
    let (hook, _) = zvcs(&root, &repo, &["config", "zvcs.hook"]);
    assert!(hook.contains("ZVCS_EVENT") && hook.contains("fired"), "local hook not set:\n{hook}");

    // Effect 2: the master switch was auto-flipped in the sandboxed global config.
    let (auto, _) = zvcs(&root, &root, &["config", "--global", "zvcs.autohook"]);
    assert_eq!(auto.trim(), "true", "autohook not auto-enabled");

    // Effect 3: the repo is indexed, so `ztrigger list` shows it.
    let (list, _) = zvcs(&root, &root, &["ztrigger", "list"]);
    assert!(list.contains("repo"), "ztrigger list missing repo:\n{list}");

    // Fire it synchronously and confirm the command ran.
    let (_t, tok) = zvcs(&root, &repo, &["ztrigger", "test", repo_s]);
    assert!(tok, "ztrigger test failed");
    let fired = std::fs::read_to_string(&marker).unwrap_or_default();
    assert!(!fired.trim().is_empty(), "trigger command did not run");

    // rm disarms: the local hook is gone and the repo drops off the armed list.
    let (_r, rok) = zvcs(&root, &root, &["ztrigger", "rm", repo_s]);
    assert!(rok, "ztrigger rm failed");
    let (list2, _) = zvcs(&root, &root, &["ztrigger", "list"]);
    assert!(!list2.contains("repo"), "repo still armed after rm:\n{list2}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zwatch_indexes_without_a_command() {
    let root = std::env::temp_dir().join(format!("zvcs-watch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("cfg/.config")).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "c0"]);
    let repo_s = repo.to_str().unwrap();

    let (_o, ok) = zvcs(&root, &root, &["zwatch", repo_s]);
    assert!(ok, "zwatch failed");

    // Indexed and listed as a plain watch (no trigger), and autostatus flipped on.
    let (list, _) = zvcs(&root, &root, &["zwatch", "list"]);
    assert!(list.contains("repo") && list.contains("watch"), "zwatch list wrong:\n{list}");
    let (auto, _) = zvcs(&root, &root, &["config", "--global", "zvcs.autostatus"]);
    assert_eq!(auto.trim(), "true", "autostatus not enabled");

    // No hook was set — it must not appear as armed.
    let (tlist, _) = zvcs(&root, &root, &["ztrigger", "list"]);
    assert!(!tlist.contains("repo"), "unexpected trigger on a plain watch:\n{tlist}");

    // rm removes it from the index.
    let (_r, rok) = zvcs(&root, &root, &["zwatch", "rm", repo_s]);
    assert!(rok, "zwatch rm failed");
    let (list2, _) = zvcs(&root, &root, &["zwatch", "list"]);
    assert!(!list2.contains("repo"), "repo still watched after rm:\n{list2}");

    let _ = std::fs::remove_dir_all(&root);
}
