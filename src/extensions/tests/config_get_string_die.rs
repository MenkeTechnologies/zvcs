//! `repo_config_get_string()` / `repo_config_get_string_tmp()` (config.c:2374-2394)
//! die through `git_die_config()` (config.c:2561-2577) when the last value of the
//! key has no `=`: `error: missing value for '<key>'`, then a `fatal:` naming the
//! command line or the file and line, exit 128. Each test here is a call site
//! whose zvcs equivalent used to read the valueless key as unset.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-getstrdie-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.ok(&["init", "-q", "-b", "main", "."]);
        std::fs::write(f.work.join("f"), "a\n").unwrap();
        f.ok(&["add", "f"]);
        f.ok(&["commit", "-qm", "m"]);
        f
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env_remove("GIT_CONFIG_GLOBAL")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_NOTES_REF")
            .env("GIT_EDITOR", "true")
            .env("GIT_PAGER", "cat")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@x")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@x")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) {
        let out = self.run(args);
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn append_config(&self, text: &str) -> usize {
        let path = self.work.join(".git/config");
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str(text);
        std::fs::write(&path, &body).unwrap();
        body.lines().count()
    }
}

fn from_command_line(key: &str) -> String {
    format!("error: missing value for '{key}'\nfatal: unable to parse '{key}' from command-line config\n")
}

fn assert_died(out: &Output, stderr: &str) {
    assert_eq!(String::from_utf8_lossy(&out.stderr), stderr, "{out:?}");
    assert_eq!(out.status.code(), Some(128));
}

/// graph.c:362, builtin/log.c:242-245 and notes.c:1009, each reached only when
/// the reader runs: `--graph`, a decoration being shown, and notes being
/// displayed (the default for `log` without a `--pretty`).
#[test]
fn log_reads_die_only_where_git_reads_them() {
    let f = Fixture::new("log");
    assert_died(&f.run(&["-c", "log.graphcolors", "log", "--graph", "--oneline"]), &from_command_line("log.graphcolors"));
    f.ok(&["-c", "log.graphcolors", "log", "--oneline"]);

    assert_died(
        &f.run(&["-c", "log.initialDecorationSet", "log", "-1", "--format=%d"]),
        &from_command_line("log.initialdecorationset"),
    );
    f.ok(&["-c", "log.initialdecorationset", "log", "-1", "--oneline"]);

    for args in [&["log", "-1"][..], &["show", "-s"], &["notes", "list"], &["log", "-1", "--notes"]] {
        let mut argv = vec!["-c", "core.notesRef"];
        argv.extend_from_slice(args);
        assert_died(&f.run(&argv), &from_command_line("core.notesref"));
    }
    f.ok(&["-c", "core.notesref", "log", "-1", "--oneline"]);
}

/// branch.c:353-364 `read_branch_desc()`, for `branch --edit-description` and
/// the format-patch cover letter, with the file origin and its line.
#[test]
fn valueless_branch_description_dies_in_both_readers() {
    let f = Fixture::new("desc");
    assert_died(
        &f.run(&["-c", "branch.main.description", "branch", "--edit-description"]),
        &from_command_line("branch.main.description"),
    );
    let line = f.append_config("[branch \"main\"]\n\tdescription\n");
    let out = f.run(&["format-patch", "--cover-letter", "-1", "--stdout"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "error: missing value for 'branch.main.description'\n\
             fatal: bad config variable 'branch.main.description' in file '.git/config' at line {line}\n"
        )
    );
    assert_eq!(out.status.code(), Some(128));
    f.ok(&["format-patch", "--cover-letter", "--cover-from-description=none", "-1", "--stdout"]);
}

/// A second repository cloned from the fixture, so `origin/<branch>` DWIM,
/// `remote`, `fetch` and `ls-remote` have something to look at.
fn with_clone(tag: &str) -> (Fixture, Fixture) {
    let up = Fixture::new(&format!("{tag}-up"));
    up.ok(&["branch", "side"]);
    let down = Fixture::new(tag);
    let _ = std::fs::remove_dir_all(&down.work);
    let out = Command::new(BIN)
        .args(["clone", "-q"])
        .arg(&up.work)
        .arg(&down.work)
        .env("HOME", &down.root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "clone: {out:?}");
    (up, down)
}

/// apply.c:50-55, sequencer.c:6180, builtin/replay.c:39, builtin/gc.c:1978,
/// add-patch.c:366-371, refs.c:700 and help.c:422.
#[test]
fn porcelain_string_readers_die_on_a_valueless_key() {
    let f = Fixture::new("porcelain");
    std::fs::write(f.work.join("f"), "b\n").unwrap();
    let patch = f.run(&["diff"]).stdout;
    f.ok(&["checkout", "--", "f"]);
    std::fs::write(f.root.join("p.diff"), patch).unwrap();
    let p = f.root.join("p.diff");
    let p = p.to_str().unwrap();

    for key in ["apply.whitespace", "apply.ignorewhitespace"] {
        assert_died(&f.run(&["-c", key, "apply", "--check", p]), &from_command_line(key));
    }
    assert_died(
        &f.run(&["-c", "rebase.instructionFormat", "rebase", "-i", "--root"]),
        &from_command_line("rebase.instructionFormat"),
    );
    assert_died(
        &f.run(&["-c", "replay.refAction", "replay", "--onto", "main", "HEAD~0..HEAD"]),
        &from_command_line("replay.refAction"),
    );
    assert_died(&f.run(&["-c", "maintenance.strategy", "maintenance", "run"]), &from_command_line("maintenance.strategy"));
    f.ok(&["-c", "maintenance.strategy", "maintenance", "run", "--task=pack-refs"]);
    for key in ["interactive.difffilter", "diff.algorithm"] {
        assert_died(&f.run(&["-c", key, "add", "-p"]), &from_command_line(key));
    }
    assert_died(&f.run(&["-c", "init.defaultbranch", "init", "-q", "new"]), &from_command_line("init.defaultbranch"));
    assert_died(
        &f.run(&["-c", "completion.commands", "--list-cmds=config"]),
        &from_command_line("completion.commands"),
    );
}

/// checkout.c:55 reads `checkout.defaultRemote` before looking at any remote;
/// builtin/remote.c:1378 reads the filter for every remote, with or without `-v`,
/// and prints a valued one.
#[test]
fn remote_readers_die_on_a_valueless_key() {
    let (_up, f) = with_clone("remote");
    for verb in ["checkout", "switch"] {
        assert_died(
            &f.run(&["-c", "checkout.defaultRemote", verb, "side"]),
            &from_command_line("checkout.defaultremote"),
        );
    }
    for args in [&["remote"][..], &["remote", "-v"]] {
        let mut argv = vec!["-c", "remote.origin.partialclonefilter"];
        argv.extend_from_slice(args);
        let out = f.run(&argv);
        assert_died(&out, &from_command_line("remote.origin.partialclonefilter"));
        assert!(out.stdout.is_empty());
    }
    let out = f.run(&["-c", "remote.origin.partialclonefilter=blob:none", "remote", "-v"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.lines().next().unwrap().ends_with(" (fetch) [blob:none]"), "{stdout}");
    assert!(stdout.lines().nth(1).unwrap().ends_with(" (push)"), "{stdout}");

    assert_died(&f.run(&["-c", "protocol.file.allow", "fetch", "origin"]), &from_command_line("protocol.file.allow"));
    assert_died(&f.run(&["-c", "protocol.allow", "fetch", "origin"]), &from_command_line("protocol.allow"));
    assert_died(&f.run(&["-c", "protocol.version", "ls-remote", "origin"]), &from_command_line("protocol.version"));
    assert_died(&f.run(&["-c", "fetch.bundleuri", "fetch", "origin"]), &from_command_line("fetch.bundleuri"));
}

/// repo-settings.c:107-130 (`prepare_repo_settings()`), repo-settings.c:184
/// (`ref_store_init()`) and repo-settings.c:204-209 (the hooks directory).
#[test]
fn repository_setting_readers_die_on_a_valueless_key() {
    let f = Fixture::new("settings");
    for key in ["core.untrackedcache", "fetch.negotiationalgorithm"] {
        assert_died(&f.run(&["-c", key, "status", "-s"]), &from_command_line(key));
    }
    assert_died(&f.run(&["-c", "core.logallrefupdates", "status", "-s"]), &from_command_line("core.logallrefupdates"));
    f.ok(&["-c", "core.logallrefupdates", "ls-files"]);
    assert_died(
        &f.run(&["-c", "core.hookspath", "commit", "-q", "--allow-empty", "-m", "x"]),
        &from_command_line("core.hookspath"),
    );
}

/// builtin/notes.c:873-886: both the valueless key and a name
/// `parse_notes_merge_strategy()` rejects die through `git_die_config()`, which
/// names the file and line.
#[test]
fn notes_merge_strategy_dies_through_git_die_config() {
    let f = Fixture::new("notesmerge");
    assert_died(
        &f.run(&["-c", "notes.x.mergeStrategy", "notes", "--ref", "x", "merge", "refs/notes/y"]),
        &from_command_line("notes.x.mergeStrategy"),
    );
    let line = f.append_config("[notes]\n\tmergeStrategy = bogus\n");
    assert_died(
        &f.run(&["notes", "merge", "refs/notes/y"]),
        &format!(
            "error: unknown notes merge strategy bogus\n\
             fatal: bad config variable 'notes.mergeStrategy' in file '.git/config' at line {line}\n"
        ),
    );
}
