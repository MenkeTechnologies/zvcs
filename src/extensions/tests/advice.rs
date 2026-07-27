//! git's `advice.*` hints are gated on their config slot. Each hint prints by
//! default and is suppressed by `advice.<slot> = false`, while the non-hint
//! lines around it always print. Regression guard for hints that advertised
//! `advice.<slot>` but never read it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new(BIN).args(args).current_dir(dir).status().unwrap().success(),
        "git {args:?} failed"
    );
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-advice-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@e.x"]);
    git(&repo, &["config", "user.name", "t"]);
    (repo, home)
}

fn run(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

/// `git add` with no pathspec: the `addEmptyPathspec` hint shows by default and
/// disappears when the slot is false; the "Nothing specified" line always shows.
#[test]
fn add_empty_pathspec_hint_is_gated() {
    let (repo, home) = fixture("emptypathspec");

    let out = run(&repo, &home, &["add"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Nothing specified, nothing added."), "err:\n{err}");
    assert!(err.contains("git add ."), "hint should show by default:\n{err}");

    git(&repo, &["config", "advice.addEmptyPathspec", "false"]);
    let out = run(&repo, &home, &["add"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Nothing specified, nothing added."), "non-hint line must remain:\n{err}");
    assert!(!err.contains("git add ."), "hint must be suppressed:\n{err}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `git branch <invalid>`: the `refSyntax` hint is gated, the fatal line is not.
#[test]
fn branch_ref_syntax_hint_is_gated() {
    let (repo, home) = fixture("refsyntax");

    let out = run(&repo, &home, &["branch", "bad..name"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a valid branch name"), "err:\n{err}");
    assert!(err.contains("check-ref-format"), "hint should show by default:\n{err}");

    git(&repo, &["config", "advice.refSyntax", "false"]);
    let out = run(&repo, &home, &["branch", "bad..name"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a valid branch name"), "fatal line must remain:\n{err}");
    assert!(!err.contains("check-ref-format"), "hint must be suppressed:\n{err}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `advise_if_enabled()` appends its `Disable this message with …` trailer only
/// while the slot is *unconfigured*. Setting `advice.refSyntax=true` keeps the
/// hint and drops the trailer — a distinction stock git makes and a hand-rolled
/// `eprintln!` pair cannot, which is what this pins.
#[test]
fn ref_syntax_trailer_appears_only_while_the_slot_is_unconfigured() {
    let (repo, home) = fixture("refsyntaxtrailer");
    const TRAILER: &str = "Disable this message with \"git config set advice.refSyntax false\"";

    let err = String::from_utf8_lossy(&run(&repo, &home, &["branch", "bad..name"]).stderr).into_owned();
    assert!(err.contains("check-ref-format"), "hint should show by default:\n{err}");
    assert!(err.contains(TRAILER), "unconfigured slot must carry the trailer:\n{err}");

    let out = run(&repo, &home, &["-c", "advice.refSyntax=true", "branch", "bad..name"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("check-ref-format"), "explicit true keeps the hint:\n{err}");
    assert!(!err.contains(TRAILER), "a configured slot must drop the trailer:\n{err}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `git switch <not-a-branch>`: `die_expecting_a_branch()` checks
/// `advice_enabled(ADVICE_SUGGEST_DETACHING_HEAD)` itself and then calls plain
/// `advise()`, so the hint is gated but never carries the disable trailer. The
/// fatal line names the ref `repo_dwim_ref()` resolved, not an object id.
#[test]
fn suggest_detaching_head_hint_is_gated_and_has_no_trailer() {
    let (repo, home) = fixture("suggestdetach");
    write(&repo, "f", "one\n");
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-qm", "one"]);

    let out = run(&repo, &home, &["switch", "HEAD"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("fatal: a branch is expected, got 'refs/heads/main'"),
        "HEAD resolves through to the branch it points at:\n{err}"
    );
    assert!(err.contains("try again with the --detach option"), "hint shows by default:\n{err}");
    assert!(
        !err.contains("Disable this message with"),
        "plain advise() prints no trailer:\n{err}"
    );

    let out = run(&repo, &home, &["-c", "advice.suggestDetachingHead=false", "switch", "HEAD"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("fatal: a branch is expected"), "fatal line must remain:\n{err}");
    assert!(!err.contains("--detach option"), "hint must be suppressed:\n{err}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// Write `name` with `body` inside `repo`.
fn write(repo: &Path, name: &str, body: &str) {
    std::fs::write(repo.join(name), body).unwrap();
}

/// `advice.statusHints` drives every "(use …)" direction in the long status
/// format *and* the wording of the trailing summary, which is the part most
/// likely to regress: git prints "no changes added to commit" without the
/// parenthetical when hints are off, not the long line minus its tail.
#[test]
fn status_hints_gate_directions_and_summary() {
    let (repo, home) = fixture("statushints");
    write(&repo, "f", "one\n");
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-qm", "one"]);
    write(&repo, "f", "two\n");
    write(&repo, "unt", "x\n");

    let out = run(&repo, &home, &["status"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("(use \"git add <file>...\" to update what will be committed)"), "{text}");
    assert!(text.contains("(use \"git restore <file>...\" to discard changes in working directory)"), "{text}");
    assert!(text.contains("(use \"git add <file>...\" to include in what will be committed)"), "{text}");
    assert!(text.contains("no changes added to commit (use \"git add\" and/or \"git commit -a\")"), "{text}");

    let out = run(&repo, &home, &["-c", "advice.statusHints=false", "status"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Changes not staged for commit:"), "section headers stay:\n{text}");
    assert!(text.contains("Untracked files:"), "section headers stay:\n{text}");
    assert!(!text.contains("(use \""), "no direction may survive:\n{text}");
    assert!(
        text.lines().any(|l| l == "no changes added to commit"),
        "summary takes its hint-less wording:\n{text}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// The unborn-repository summary has its own hint-less wording ("nothing to
/// commit"), which is a different string from the populated-repository one.
#[test]
fn status_hints_gate_unborn_summary() {
    let (repo, home) = fixture("statushintsunborn");

    let out = run(&repo, &home, &["-c", "advice.statusHints=false", "status"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("No commits yet"), "{text}");
    assert!(
        text.lines().any(|l| l == "nothing to commit"),
        "unborn summary drops its parenthetical entirely:\n{text}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `git tag -a <new> <tag>` warns that the new tag points at a tag. The trailer
/// git appends only for an unconfigured slot is part of the contract: setting
/// the slot to true keeps the hint but drops the trailer.
#[test]
fn nested_tag_hint_is_gated_and_trailer_tracks_configuration() {
    let (repo, home) = fixture("nestedtag");
    write(&repo, "f", "one\n");
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-qm", "one"]);
    git(&repo, &["tag", "-a", "-m", "one", "v1"]);

    let out = run(&repo, &home, &["tag", "-a", "-m", "two", "v2", "v1"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("You have created a nested tag."), "{err}");
    assert!(err.contains("\tgit tag -f v2 v1^{}"), "suggestion names both refs:\n{err}");
    assert!(
        err.contains("Disable this message with \"git config set advice.nestedTag false\""),
        "unconfigured slot gets the trailer:\n{err}"
    );

    let out = run(&repo, &home, &["-c", "advice.nestedTag=true", "tag", "-a", "-m", "t", "v3", "v1"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("You have created a nested tag."), "{err}");
    assert!(!err.contains("Disable this message"), "explicit true drops the trailer:\n{err}");

    let out = run(&repo, &home, &["-c", "advice.nestedTag=false", "tag", "-a", "-m", "t", "v4", "v1"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).is_empty(), "slot false is silent");

    // A lightweight tag never reaches git's create_tag, so it never warns.
    let out = run(&repo, &home, &["tag", "lw", "v1"]);
    assert!(String::from_utf8_lossy(&out.stderr).is_empty(), "lightweight tags are silent");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// A rejected push prints one advice block, gated by its own slot, by the
/// umbrella `advice.pushUpdateRejected`, and by the historical
/// `advice.pushNonFastForward` alias that git ANDs into the umbrella.
#[test]
fn push_rejection_advice_is_gated_by_slot_umbrella_and_alias() {
    let (repo, home) = fixture("pushnonff");
    let bare = repo.parent().unwrap().join("bare.git");
    git(&repo, &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
    write(&repo, "f", "one\n");
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-qm", "one"]);
    write(&repo, "f", "two\n");
    git(&repo, &["commit", "-qam", "two"]);
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&repo, &["push", "origin", "main"]);
    // Rewind the local branch so its tip is an ancestor of the remote's.
    git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);

    let out = run(&repo, &home, &["push", "origin", "main"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("! [rejected]"), "the rejection itself:\n{err}");
    assert!(
        err.contains("hint: Updates were rejected because the tip of your current branch is behind"),
        "current-branch advice by default:\n{err}"
    );
    assert!(
        err.contains("hint: See the 'Note about fast-forwards' in 'git push --help' for details."),
        "advice block is complete:\n{err}"
    );

    for slot in ["advice.pushNonFFCurrent=false", "advice.pushUpdateRejected=false", "advice.pushNonFastForward=false"] {
        let out = run(&repo, &home, &["-c", slot, "push", "origin", "main"]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("! [rejected]"), "{slot}: rejection must remain:\n{err}");
        assert!(!err.contains("hint:"), "{slot}: advice must be suppressed:\n{err}");
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `git rm <path-outside-the-sparse-checkout>`: `builtin/rm.c` leaves such an
/// entry out of the removal list, so the pathspec matches nothing and git reports
/// it through `advise_on_updating_sparse_paths()` instead of dying. The
/// three-line preamble and the path list are ungated writes; only the closing
/// suggestion is behind `advice.updateSparsePath`. Either way the entry survives
/// and the command exits 1 — which `--sparse` reverses.
#[test]
fn update_sparse_path_report_is_gated_and_leaves_the_entry_alone() {
    let (repo, home) = fixture("updatesparsepath");
    std::fs::create_dir_all(repo.join("alpha")).unwrap();
    std::fs::create_dir_all(repo.join("beta")).unwrap();
    write(&repo, "alpha/f", "in\n");
    write(&repo, "beta/f", "out\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "one"]);
    git(&repo, &["sparse-checkout", "set", "--no-cone", "alpha"]);

    const PREAMBLE: &str = "outside of your sparse-checkout definition";
    const HINT: &str = "* Use the --sparse option.";
    const TRAILER: &str =
        "Disable this message with \"git config set advice.updateSparsePath false\"";

    let out = run(&repo, &home, &["rm", "beta/f"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "the report is a failure status:\n{err}");
    assert!(err.contains(PREAMBLE), "preamble shows by default:\n{err}");
    assert!(err.contains("\nbeta/f\n"), "the offending pathspec is listed:\n{err}");
    assert!(err.contains(HINT), "the suggestion shows by default:\n{err}");
    assert!(err.contains(TRAILER), "an unconfigured slot carries the trailer:\n{err}");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("rm '"),
        "nothing may be removed"
    );

    let out = run(&repo, &home, &["-c", "advice.updateSparsePath=false", "rm", "beta/f"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "the status does not depend on the slot:\n{err}");
    assert!(err.contains(PREAMBLE), "the preamble is not gated on the slot:\n{err}");
    assert!(!err.contains(HINT), "a false slot drops the suggestion:\n{err}");

    // `--sparse` puts the entry back in scope: it is removed and nothing is said.
    let out = run(&repo, &home, &["rm", "--sparse", "beta/f"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "--sparse succeeds:\n{err}");
    assert!(err.is_empty(), "--sparse prints no report:\n{err}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("rm 'beta/f'"),
        "--sparse removes the entry"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

/// `GIT_ADVICE=0` squelches every hint whatever the config says.
#[test]
fn git_advice_env_squelches_all_hints() {
    let (repo, home) = fixture("gitadviceenv");
    write(&repo, "f", "one\n");
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-qm", "one"]);
    git(&repo, &["tag", "-a", "-m", "one", "v1"]);

    let out = Command::new(BIN)
        .args(["tag", "-a", "-m", "two", "v2", "v1"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("ZVCS_HOME", &home)
        .env("GIT_ADVICE", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "GIT_ADVICE=0 must silence the nestedTag hint"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
