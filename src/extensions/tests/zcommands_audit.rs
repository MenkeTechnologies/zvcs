//! `git zcommands` and `git zaudit` — the fleet command log, and the query
//! layer over it.
//!
//! Neither verb had an integration test. Between them they are the project's
//! accountability story — "which agent ran `push --force`, in which repo, and
//! when" — and that story rests on three properties that no unit test covers,
//! because all three live in the interaction between `dispatch::run` and the
//! log file:
//!
//!  * **Logging is off until asked for.** The cost on every git command run on
//!    the machine is one `stat` of a marker file. If the marker ever exists by
//!    default, or the log is written when it does not, every command everywhere
//!    starts paying for a feature nobody enabled.
//!  * **Once on, every command lands** — with the fields the audit layer parses
//!    back out. `zaudit` splits on tabs into `ts pid ppid cwd argv`; a writer
//!    that emits four fields and a reader that wants five agree on nothing.
//!  * **The feed does not log itself.** `zcommands` reads the log while
//!    following it, so recording its own invocation would make the feed grow
//!    from being watched.
//!
//! `--no-follow` is mandatory in every case here: without it the verb tails the
//! log forever and the test never returns.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
        .output()
        .unwrap()
}

fn out_of(o: &Output) -> String {
    let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&o.stderr));
    s
}

fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zcmd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(run(&repo, &home, &["init", "-q", "-b", "main", "."]).status.success());
    std::fs::write(repo.join("a.txt"), b"one\n").unwrap();
    assert!(run(&repo, &home, &["add", "a.txt"]).status.success());
    assert!(run(&repo, &home, &["commit", "-q", "-m", "first"]).status.success());
    (repo.canonicalize().unwrap(), home)
}

/// Every line currently in the log.
fn log_lines(home: &Path) -> Vec<String> {
    std::fs::read_to_string(home.join("zvcs/commands.log"))
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[test]
fn nothing_is_logged_until_the_feed_is_asked_for() {
    let (repo, home) = fixture("off");
    // The fixture already ran init/add/commit through the binary.
    assert!(!home.join("zvcs/commands.enabled").exists(), "the marker exists by default");
    assert!(log_lines(&home).is_empty(), "commands were logged with logging off");

    run(&repo, &home, &["status", "--porcelain"]);
    run(&repo, &home, &["log", "--oneline"]);
    assert!(log_lines(&home).is_empty(), "commands were logged with logging off");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn enabling_records_each_command_in_the_shape_the_audit_layer_parses() {
    let (repo, home) = fixture("on");
    let enable = run(&repo, &home, &["zcommands", "--no-follow"]);
    assert!(enable.status.success(), "{}", out_of(&enable));
    assert!(home.join("zvcs/commands.enabled").exists(), "no marker after enabling");

    run(&repo, &home, &["status", "--porcelain"]);
    run(&repo, &home, &["log", "--oneline"]);

    let lines = log_lines(&home);
    assert!(lines.len() >= 2, "expected both commands, got {lines:?}");

    // The record shape `zaudit::parse` splits back out: ts, pid, ppid, cwd, argv.
    let last = lines.last().unwrap();
    let f: Vec<&str> = last.splitn(5, '\t').collect();
    assert_eq!(f.len(), 5, "record is not five tab-separated fields: {last:?}");
    assert!(f[0].parse::<i64>().is_ok(), "field 1 is not a timestamp: {last:?}");
    assert!(f[1].parse::<u32>().is_ok(), "field 2 is not a pid: {last:?}");
    assert!(f[2].parse::<i32>().is_ok(), "field 3 is not a ppid: {last:?}");
    assert!(f[3].contains(repo.file_name().unwrap().to_str().unwrap()), "field 4 is not the cwd: {last:?}");
    assert_eq!(f[4], "log --oneline", "field 5 is not the argv: {last:?}");

    // And the backlog reader shows them.
    let feed = out_of(&run(&repo, &home, &["zcommands", "--no-follow"]));
    assert!(feed.contains("log --oneline"), "the backlog omits a logged command:\n{feed}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn the_feed_does_not_record_itself() {
    // Reading the feed must not append to it, or watching would make it grow.
    let (repo, home) = fixture("selfless");
    run(&repo, &home, &["zcommands", "--no-follow"]);
    run(&repo, &home, &["status", "--porcelain"]);
    let before = log_lines(&home).len();

    run(&repo, &home, &["zcommands", "--no-follow"]);
    run(&repo, &home, &["zcommands", "-n", "5", "--no-follow"]);
    let after = log_lines(&home);
    assert_eq!(after.len(), before, "the feed logged its own invocation:\n{after:?}");
    assert!(
        !after.iter().any(|l| l.contains("\tzcommands")),
        "a zcommands record reached the log:\n{after:?}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn off_stops_recording_and_clear_empties_without_stopping() {
    let (repo, home) = fixture("offclear");
    run(&repo, &home, &["zcommands", "--no-follow"]);
    run(&repo, &home, &["status", "--porcelain"]);
    assert!(!log_lines(&home).is_empty());

    // `--clear` truncates but leaves logging on.
    run(&repo, &home, &["zcommands", "--clear"]);
    assert!(log_lines(&home).is_empty(), "clear did not empty the log");
    assert!(home.join("zvcs/commands.enabled").exists(), "clear also disabled logging");
    run(&repo, &home, &["log", "--oneline"]);
    assert_eq!(log_lines(&home).len(), 1, "logging did not continue after clear");

    // `--off` removes the marker, and nothing is recorded afterwards.
    run(&repo, &home, &["zcommands", "--off"]);
    assert!(!home.join("zvcs/commands.enabled").exists(), "--off left the marker");
    let frozen = log_lines(&home).len();
    run(&repo, &home, &["status", "--porcelain"]);
    run(&repo, &home, &["log", "--oneline"]);
    assert_eq!(log_lines(&home).len(), frozen, "commands were logged after --off");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn zaudit_filters_the_same_log_by_command_and_by_mutation() {
    let (repo, home) = fixture("audit");
    run(&repo, &home, &["zcommands", "--no-follow"]);

    // A read and a write, so `--mutating` has one of each to separate.
    run(&repo, &home, &["status", "--porcelain"]);
    std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
    run(&repo, &home, &["commit", "-qam", "second"]);
    run(&repo, &home, &["log", "--oneline"]);

    let all = out_of(&run(&repo, &home, &["zaudit"]));
    assert!(all.contains("status"), "zaudit omits a logged read:\n{all}");
    assert!(all.contains("commit"), "zaudit omits a logged write:\n{all}");

    // `--cmd` keeps one verb.
    let only_commit = out_of(&run(&repo, &home, &["zaudit", "--cmd", "commit"]));
    assert!(only_commit.contains("commit"), "{only_commit}");
    assert!(!only_commit.contains("status --porcelain"), "--cmd let another verb through:\n{only_commit}");

    // `--mutating` keeps the write and drops the reads — the filter that makes
    // the trail answerable ("what changed state, and who did it").
    let mutating = out_of(&run(&repo, &home, &["zaudit", "--mutating"]));
    assert!(mutating.contains("commit"), "--mutating dropped a write:\n{mutating}");
    assert!(!mutating.contains("status --porcelain"), "--mutating kept a read:\n{mutating}");
    assert!(!mutating.contains("log --oneline"), "--mutating kept a read:\n{mutating}");

    // `--summary` aggregates rather than listing.
    let summary = out_of(&run(&repo, &home, &["zaudit", "--summary"]));
    assert!(summary.contains("commit"), "the summary omits a command it counted:\n{summary}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn json_output_is_one_object_per_command() {
    // Both verbs advertise NDJSON for tooling; a trailing array or a pretty
    // printer would break `jq -c` streaming, and nothing else checks the shape.
    let (repo, home) = fixture("json");
    run(&repo, &home, &["zcommands", "--no-follow"]);
    run(&repo, &home, &["status", "--porcelain"]);
    run(&repo, &home, &["log", "--oneline"]);

    for args in [&["zcommands", "--json", "--no-follow"][..], &["zaudit", "--json"]] {
        let out = String::from_utf8_lossy(&run(&repo, &home, args).stdout).into_owned();
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        assert!(!lines.is_empty(), "{args:?} produced no JSON:\n{out}");
        for l in &lines {
            assert!(l.starts_with('{') && l.ends_with('}'), "{args:?} line is not one object: {l}");
            assert!(l.contains("\"argv\""), "{args:?} line has no argv field: {l}");
        }
    }

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
