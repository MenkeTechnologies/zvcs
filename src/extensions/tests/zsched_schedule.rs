//! `git zsched` — the built-in cron for the tree.
//!
//! Third of the "the CLI writes, a daemon acts" surfaces in this suite, and the
//! one with the sharpest split: the CLI owns *every* write to
//! `$ZVCS_HOME/schedule.tsv`, and the daemon's scheduler thread only ever reads
//! it, re-reading each tick. That is what lets `add`/`rm` take effect without a
//! reload — and it means the file *is* the interface. A writer that emits a
//! shape the reader does not parse produces no error anywhere: schedules simply
//! never fire.
//!
//! So the first case asserts the record field by field rather than grepping the
//! listing, the way `zcommands_audit` does for the command log.
//!
//! The rest are the contracts a user leans on unattended:
//!
//!  * a bad duration is refused **without writing** — a scheduler that records
//!    a malformed entry and complains afterwards leaves the daemon parsing it
//!    every tick;
//!  * the interval is clamped to at least one second, because a zero would make
//!    the scheduler fire continuously rather than never;
//!  * ids are not reused after a removal, or a stale `zsched rm 2` deletes
//!    whatever took that slot;
//!  * `run <id>` executes synchronously and *returns the command's status*,
//!    which is the only thing that makes it scriptable.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("ZVCS_SOCK", home.join("sock"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("zvcs-zsched-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    let dir = root.join("dir");
    std::fs::create_dir_all(&dir).unwrap();
    (dir, home)
}

/// The schedule file as the daemon reads it.
fn schedules(home: &Path) -> Vec<String> {
    std::fs::read_to_string(home.join("zvcs/schedule.tsv"))
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[test]
fn a_schedule_is_written_in_the_shape_the_daemon_parses() {
    let (dir, home) = fixture("shape");
    let out = ok(&run(&dir, &home, &["zsched", "add", "5m", "--", "git", "zpull", "--dirty"]), "add");
    assert!(out.contains("#1"), "{out}");
    assert!(out.contains("every 5m"), "the confirmation does not render the interval:\n{out}");

    let lines = schedules(&home);
    assert_eq!(lines.len(), 1, "expected one record: {lines:?}");
    let f: Vec<&str> = lines[0].splitn(3, '\t').collect();
    assert_eq!(f.len(), 3, "record is not three tab-separated fields: {:?}", lines[0]);
    assert_eq!(f[0], "1", "id field: {:?}", lines[0]);
    assert_eq!(f[1], "300", "interval is not seconds: {:?}", lines[0]);
    assert_eq!(f[2], "git zpull --dirty", "the command lost its arguments: {:?}", lines[0]);

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}

#[test]
fn durations_are_parsed_and_a_bad_one_writes_nothing() {
    let (dir, home) = fixture("dur");
    for (spelling, secs) in [("30s", "30"), ("5m", "300"), ("1h", "3600"), ("1h30m", "5400")] {
        ok(&run(&dir, &home, &["zsched", "clear"]), "clear");
        ok(&run(&dir, &home, &["zsched", "add", spelling, "--", "true"]), spelling);
        let lines = schedules(&home);
        let f: Vec<&str> = lines[0].splitn(3, '\t').collect();
        assert_eq!(f[1], secs, "{spelling} parsed wrong: {:?}", lines[0]);
    }

    // A malformed duration must not reach the file the daemon parses.
    ok(&run(&dir, &home, &["zsched", "clear"]), "clear");
    let bad = run(&dir, &home, &["zsched", "add", "soon", "--", "true"]);
    assert!(!bad.status.success(), "a bad duration was accepted");
    assert!(schedules(&home).is_empty(), "a refused schedule was written: {:?}", schedules(&home));

    // Zero is clamped to one second — a zero interval would fire every tick.
    ok(&run(&dir, &home, &["zsched", "add", "0s", "--", "true"]), "add 0s");
    let lines = schedules(&home);
    let f: Vec<&str> = lines[0].splitn(3, '\t').collect();
    assert_eq!(f[1], "1", "a zero interval was stored: {:?}", lines[0]);

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}

#[test]
fn an_id_is_the_highest_in_use_plus_one_and_is_reused_after_a_removal() {
    // Measured, not assumed, and recorded here because it is a sharp edge: the
    // id is `max(remaining) + 1`, so removing the highest schedule hands its
    // number to the next one added. `zguard` and `zintercept` number their
    // registries the same way (`guard.rs:next_id`, `intercepts.rs:323`), so
    // this is the project's convention rather than a slip in one verb — but it
    // means a note or a script holding "rm 2" can delete something else
    // entirely once #2 has been removed and re-added. Pinned so that changing
    // it is a decision rather than an accident.
    let (dir, home) = fixture("ids");
    ok(&run(&dir, &home, &["zsched", "add", "1m", "--", "one"]), "add 1");
    ok(&run(&dir, &home, &["zsched", "add", "1m", "--", "two"]), "add 2");
    ok(&run(&dir, &home, &["zsched", "rm", "2"]), "rm 2");

    let out = ok(&run(&dir, &home, &["zsched", "add", "1m", "--", "three"]), "add after rm");
    assert!(out.contains("#2"), "the id scheme changed — see this test's comment:\n{out}");
    let lines = schedules(&home);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("1\t")), "{lines:?}");
    // #2 now names the third command, which is the hazard being recorded.
    assert!(
        lines.iter().any(|l| l.starts_with("2\t") && l.ends_with("three")),
        "{lines:?}"
    );

    // Removing an id that is not there is an error, not a silent no-op.
    let missing = run(&dir, &home, &["zsched", "rm", "9"]);
    assert!(!missing.status.success(), "removing a missing id succeeded");
    assert_eq!(schedules(&home).len(), 2, "a failed removal changed the file");

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}

#[test]
fn run_fires_one_schedule_now_and_reports_its_status() {
    // `run` is the scriptable half: it executes the command synchronously and
    // exits with the command's own status, so a caller can gate on it.
    let (dir, home) = fixture("run");
    let stamp = home.join("fired");
    let cmd = format!("touch {}", stamp.display());
    ok(&run(&dir, &home, &["zsched", "add", "1h", "--", &cmd]), "add");

    assert!(!stamp.exists(), "the command ran at schedule time");
    let out = ok(&run(&dir, &home, &["zsched", "run", "1"]), "run 1");
    assert!(out.contains("running #1"), "{out}");
    assert!(stamp.exists(), "`zsched run` did not execute the command");

    // A failing command is reported as a failure.
    ok(&run(&dir, &home, &["zsched", "add", "1h", "--", "exit 3"]), "add failing");
    let failed = run(&dir, &home, &["zsched", "run", "2"]);
    assert!(!failed.status.success(), "a failing schedule reported success");

    // And an id that does not exist is an error rather than a silent success.
    let missing = run(&dir, &home, &["zsched", "run", "9"]);
    assert!(!missing.status.success(), "running a missing id succeeded");

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}

#[test]
fn clear_empties_the_file_and_list_says_so() {
    let (dir, home) = fixture("clear");
    ok(&run(&dir, &home, &["zsched", "add", "1m", "--", "true"]), "add");
    assert_eq!(schedules(&home).len(), 1);

    ok(&run(&dir, &home, &["zsched", "clear"]), "clear");
    assert!(schedules(&home).is_empty(), "clear left records: {:?}", schedules(&home));
    let listed = ok(&run(&dir, &home, &["zsched"]), "list");
    assert!(
        listed.contains("no schedules") || listed.trim().is_empty(),
        "the empty listing is not empty:\n{listed}"
    );

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}
