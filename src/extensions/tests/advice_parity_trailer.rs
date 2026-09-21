//! `advise_if_enabled()` prints its `Disable this message with …` trailer only
//! while the slot is *unconfigured* — `vadvise()` is handed
//! `!advice_setting[type].level` as `display_instructions` (advice.c:157), and an
//! explicit `advice.<slot> = true` raises that level to `ADVICE_LEVEL_ENABLED`.
//! So `advice.<slot>=true` keeps the hint and drops the trailer, while unset
//! keeps both. Every site that spells the trailer out by hand instead of going
//! through the shared gate gets this wrong in the same direction: it prints the
//! trailer unconditionally.
//!
//! Each case here was measured against stock git 2.55.0 in all three states
//! (unset / `=true` / `=false`) before being written down.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_ADVICE")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn err_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A repository with one commit, an isolated `HOME`, and `main` as the branch so
/// nothing here depends on the compiled-in initial-branch default.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-advtrail-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let root = root.canonicalize().unwrap();
    let (home, repo) = (root.join("home"), root.join("repo"));
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-qm", "one"]);
    (repo, home)
}

fn cleanup(repo: &Path) {
    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The three states of one slot, over one command: the hint body must survive an
/// explicit `true`, the trailer must not, and `false` must take both away.
fn assert_trailer_tracks_configuration(
    repo: &Path,
    home: &Path,
    slot: &str,
    body: &str,
    args: &[&str],
) {
    let trailer = format!("Disable this message with \"git config set {slot} false\"");

    let err = err_of(&run(repo, home, args));
    assert!(err.contains(body), "{slot}: hint must show while unconfigured:\n{err}");
    assert!(err.contains(&trailer), "{slot}: unconfigured slot must carry the trailer:\n{err}");

    let mut on = vec!["-c".to_string(), format!("{slot}=true")];
    on.extend(args.iter().map(|a| a.to_string()));
    let on: Vec<&str> = on.iter().map(String::as_str).collect();
    let err = err_of(&run(repo, home, &on));
    assert!(err.contains(body), "{slot}: explicit true must keep the hint:\n{err}");
    assert!(!err.contains(&trailer), "{slot}: a configured slot must drop the trailer:\n{err}");

    let mut off = vec!["-c".to_string(), format!("{slot}=false")];
    off.extend(args.iter().map(|a| a.to_string()));
    let off: Vec<&str> = off.iter().map(String::as_str).collect();
    let err = err_of(&run(repo, home, &off));
    assert!(!err.contains(body), "{slot}: false must suppress the hint:\n{err}");
    assert!(!err.contains(&trailer), "{slot}: false must suppress the trailer too:\n{err}");
}

/// `cmd_add()`'s `Nothing specified, nothing added.` (builtin/add.c:466-471) is a
/// plain stderr line; only the `git add .` suggestion is the hint. `git stage`
/// is the same code path and must answer identically.
#[test]
fn add_empty_pathspec_trailer_tracks_configuration() {
    let (repo, home) = fixture("emptypathspec");
    for verb in ["add", "stage"] {
        assert_trailer_tracks_configuration(
            &repo,
            &home,
            "advice.addEmptyPathspec",
            "Maybe you wanted to say 'git add .'?",
            &[verb],
        );
        let err = err_of(&run(&repo, &home, &["-c", "advice.addEmptyPathspec=true", verb]));
        assert!(
            err.contains("Nothing specified, nothing added."),
            "{verb}: the non-hint line is not gated:\n{err}"
        );
    }
    cleanup(&repo);
}

/// `add_files()` (builtin/add.c:347-353): the preamble and the path list are
/// plain stderr writes, the `Use -f` line is the only `advise_if_enabled()`.
#[test]
fn add_ignored_file_trailer_tracks_configuration() {
    let (repo, home) = fixture("ignoredfile");
    std::fs::write(repo.join(".gitignore"), "ign\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-qm", "ig"]);
    std::fs::write(repo.join("ign"), "z\n").unwrap();

    assert_trailer_tracks_configuration(
        &repo,
        &home,
        "advice.addIgnoredFile",
        "Use -f if you really want to add them.",
        &["add", "ign"],
    );
    let err = err_of(&run(&repo, &home, &["-c", "advice.addIgnoredFile=false", "add", "ign"]));
    assert!(
        err.contains("The following paths are ignored by one of your .gitignore files:")
            && err.contains("ign"),
        "the report itself is not gated on the slot:\n{err}"
    );
    cleanup(&repo);
}

/// A sparse-checkout fixture: `in/` is in the cone, `out/` is not.
fn sparse_fixture(tag: &str) -> (PathBuf, PathBuf) {
    let (repo, home) = fixture(tag);
    std::fs::create_dir_all(repo.join("in")).unwrap();
    std::fs::create_dir_all(repo.join("out")).unwrap();
    std::fs::write(repo.join("in/f"), "i\n").unwrap();
    std::fs::write(repo.join("out/f"), "o\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "two"]);
    git(&repo, &["sparse-checkout", "init", "--cone", "--sparse-index"]);
    git(&repo, &["sparse-checkout", "set", "in"]);
    (repo, home)
}

/// `advise_on_updating_sparse_paths()` (advice.c:262-272): the three-line
/// preamble and the path list print unconditionally, the closing block is the
/// hint.
#[test]
fn update_sparse_path_trailer_tracks_configuration() {
    let (repo, home) = sparse_fixture("updatesparse");
    std::fs::write(repo.join("out/f"), "z\n").unwrap();

    assert_trailer_tracks_configuration(
        &repo,
        &home,
        "advice.updateSparsePath",
        "If you intend to update such entries, try one of the following:",
        &["add", "out/f"],
    );
    let out = run(&repo, &home, &["-c", "advice.updateSparsePath=false", "add", "out/f"]);
    let err = err_of(&out);
    assert!(
        err.contains("outside of your sparse-checkout definition, so will not be"),
        "the report itself is not gated on the slot:\n{err}"
    );
    assert_eq!(out.status.code(), Some(1), "advice must not move the exit code:\nerr:\n{err}");
    cleanup(&repo);
}

/// `die_user_resolve()` (builtin/am.c:1161-1184) composes the whole block into
/// one `strbuf` and passes it to a *single* `advise_if_enabled()`, so one trailer
/// decision covers all of the lines — and the `--allow-empty` line inside, the
/// one part gated on `advice.amWorkDir`, does not get a trailer of its own.
///
/// A failed `git am` leaves `.git/rebase-apply` behind, so each state is run on
/// a freshly aborted tree.
#[test]
fn am_merge_conflict_trailer_tracks_configuration() {
    let (repo, home) = fixture("amresolve");
    std::fs::write(repo.join("a.txt"), "a\nb\n").unwrap();
    git(&repo, &["commit", "-qam", "two"]);
    git(&repo, &["format-patch", "-q", "-1", "-o", "."]);
    let patch = std::fs::read_dir(&repo)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(".patch"))
        .expect("format-patch wrote a patch");

    const BODY: &str = "When you have resolved this problem, run \"git am --continue\".";
    const TRAILER: &str = "Disable this message with \"git config set advice.mergeConflict false\"";

    let attempt = |extra: &[&str]| -> Output {
        std::fs::write(repo.join("a.txt"), "a\ndirty\n").unwrap();
        let mut args: Vec<&str> = extra.to_vec();
        args.extend_from_slice(&["am", &patch]);
        let out = run(&repo, &home, &args);
        let _ = run(&repo, &home, &["am", "--abort"]);
        out
    };

    let out = attempt(&[]);
    let err = err_of(&out);
    assert!(err.contains(BODY), "unconfigured slot must print the block:\n{err}");
    assert!(err.contains(TRAILER), "unconfigured slot must carry the trailer:\n{err}");
    assert_eq!(out.status.code(), Some(128), "exit code is the `die(NULL)`:\n{err}");

    let err = err_of(&attempt(&["-c", "advice.mergeConflict=true"]));
    assert!(err.contains(BODY), "explicit true must keep the block:\n{err}");
    assert!(!err.contains(TRAILER), "a configured slot must drop the trailer:\n{err}");

    let out = attempt(&["-c", "advice.mergeConflict=false"]);
    let err = err_of(&out);
    assert!(!err.contains(BODY), "false must suppress the block:\n{err}");
    assert!(!err.contains(TRAILER), "false must suppress the trailer too:\n{err}");
    assert!(
        !err.contains("--abort") && !err.contains("--skip"),
        "no part of the die_user_resolve block may survive:\n{err}"
    );
    // The `Use 'git am --show-current-patch=diff'` line is a different slot
    // (`advice.amWorkDir`, builtin/am.c:1913-1914) and is unaffected.
    assert!(
        err.contains("Use 'git am --show-current-patch=diff' to see the failed patch"),
        "advice.mergeConflict must not reach the amWorkDir hint:\n{err}"
    );
    assert_eq!(out.status.code(), Some(128), "advice must not move the exit code:\n{err}");

    cleanup(&repo);
}

/// `repo_default_branch_name()` (refs.c:703-712) hints only when it reaches its
/// compiled-in fallback: an explicit `-b`, a configured `init.defaultBranch` and
/// `git init -q` each return before the `advise_if_enabled()`.
#[test]
fn default_branch_name_hint_fires_only_on_the_compiled_in_fallback() {
    let root =
        std::env::temp_dir().join(format!("zvcs-advtrail-defbranch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    const FIRST: &str = "Using 'master' as the name for the initial branch. This default branch name";
    const LAST: &str = "\tgit branch -m <name>";
    const TRAILER: &str =
        "Disable this message with \"git config set advice.defaultBranchName false\"";

    let init = |args: &[&str], tag: &str| -> String {
        let dir = root.join(tag);
        std::fs::create_dir_all(&dir).unwrap();
        let mut a: Vec<&str> = args.to_vec();
        a.push(".");
        err_of(&run(&dir, &home, &a))
    };

    let err = init(&["init"], "plain");
    for line in [FIRST, "\tgit config --global init.defaultBranch <name>", LAST, TRAILER] {
        assert!(err.contains(line), "fallback init must hint {line:?}:\n{err}");
    }
    assert!(
        err.lines().all(|l| l.starts_with("hint:")),
        "every advice line carries the hint: prefix:\n{err}"
    );
    // `vadvise()` walks one buffer, so the body's trailing newline is what puts a
    // bare `hint:` between the block and the trailer.
    assert!(err.contains("\nhint:\nhint: Disable this message"), "blank hint line:\n{err}");

    let err = init(&["-c", "advice.defaultBranchName=true", "init"], "on");
    assert!(err.contains(FIRST) && err.contains(LAST), "explicit true keeps the hint:\n{err}");
    assert!(!err.contains(TRAILER), "a configured slot drops the trailer:\n{err}");

    let err = init(&["-c", "advice.defaultBranchName=false", "init"], "off");
    assert!(!err.contains("hint:"), "false suppresses the whole block:\n{err}");

    for (args, tag, why) in [
        (vec!["init", "-q"], "quiet", "-q passes quiet=1 to repo_default_branch_name"),
        (vec!["init", "-b", "zz"], "explicitb", "-b returns before the fallback"),
        (
            vec!["-c", "init.defaultBranch=qq", "init"],
            "configured",
            "a configured init.defaultBranch returns before the fallback",
        ),
    ] {
        let dir = root.join(tag);
        std::fs::create_dir_all(&dir).unwrap();
        let mut a = args.clone();
        a.push(".");
        let err = err_of(&run(&dir, &home, &a));
        assert!(!err.contains("hint:"), "{why}:\n{err}");
    }

    let dir = root.join("envoff");
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(BIN)
        .args(["init", "."])
        .current_dir(&dir)
        .env("HOME", &home)
        .env("ZVCS_HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ADVICE", "0")
        .output()
        .unwrap();
    assert!(!err_of(&out).contains("hint:"), "GIT_ADVICE=0 squelches it:\n{}", err_of(&out));

    let _ = std::fs::remove_dir_all(&root);
}
