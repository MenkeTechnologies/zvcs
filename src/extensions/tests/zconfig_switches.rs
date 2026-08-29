//! `git zconfig` — the daemon's autonomy switches, read and written from the CLI.
//!
//! This verb writes configuration that a *background process* acts on, which is
//! what makes it worth a test even though it looks like a thin wrapper: a
//! mistake here is not a wrong line of output, it is autonomy silently running
//! (or silently not running) on somebody's machine. Three properties carry that
//! risk and none had a test:
//!
//!  * **Writes go to the global config.** The daemon reads `~/.gitconfig` on
//!    start and reload. A write that landed in the repository's own config
//!    would look identical in `git zconfig`'s listing and do nothing at all.
//!  * **`all off` spares the settings it is not meant to touch.** The table
//!    marks `interval` as ungated: it is the autonomy *debounce*, always on,
//!    and setting it to 0 does not disable a loop — it removes the delay from
//!    one. `all off` must skip it while turning off every gated switch.
//!  * **A count is not a boolean.** `statusinterval` and `watchmru` gate their
//!    loops with `0`; accepting `on` for one of them would write a value the
//!    daemon cannot parse.
//!
//! Every case runs with `HOME` inside the fixture, so the "global config" under
//! test is the fixture's own and the developer's real `~/.gitconfig` is neither
//! read nor written.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn run(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("ZVCS_HOME", home.join("zvcs"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e.x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e.x")
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
    let root = std::env::temp_dir().join(format!("zvcs-zconfig-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("zvcs")).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    ok(&run(&repo, &home, &["init", "-q", "-b", "main", "."]), "init");
    (repo, home)
}

/// The rows of a `zconfig` listing that are marked as set (`*`).
///
/// A row is a line that opens with the marker *and* names a setting: the
/// listing ends with a legend line that also starts with `*`, and counting that
/// as a marked row made the first version of this test fail against correct
/// output.
fn marked_rows(listing: &str) -> Vec<&str> {
    listing
        .lines()
        .filter(|l| l.trim_start().starts_with('*'))
        .filter(|l| SETTINGS.iter().any(|k| l.contains(k)))
        .collect()
}

/// Every setting the table defines, in listing order.
const SETTINGS: &[&str] = &[
    "autoreconcile",
    "autobump",
    "autocrawl",
    "autostatus",
    "autohook",
    "autodups",
    "statusinterval",
    "watchmru",
    "interval",
];

/// The global config as text, which is where the daemon looks.
fn global(home: &Path) -> String {
    std::fs::read_to_string(home.join(".gitconfig")).unwrap_or_default()
}

/// The repository's own config, which the daemon does *not* read for these.
fn repo_config(repo: &Path) -> String {
    std::fs::read_to_string(repo.join(".git/config")).unwrap_or_default()
}

#[test]
fn the_listing_shows_every_setting_and_marks_only_what_is_set() {
    let (repo, home) = fixture("list");
    let out = ok(&run(&repo, &home, &["zconfig"]), "zconfig");
    for key in SETTINGS {
        assert!(out.contains(key), "the listing omits {key}:\n{out}");
    }
    // Nothing is set yet, so no row is marked. The trailing legend also opens
    // with `*`, so a row is identified by carrying a setting name as well.
    assert!(marked_rows(&out).is_empty(), "an unset row was marked:\n{out}");

    ok(&run(&repo, &home, &["zconfig", "autobump", "on"]), "set autobump");
    let out = ok(&run(&repo, &home, &["zconfig"]), "zconfig after set");
    let marked = marked_rows(&out);
    assert_eq!(marked.len(), 1, "expected exactly one marked row:\n{out}");
    assert!(marked[0].contains("autobump"), "the wrong row is marked: {marked:?}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_switch_is_written_to_the_global_config_the_daemon_reads() {
    // The failure this catches is invisible in the listing: a value written to
    // the repository's config reads back the same way and the daemon never
    // sees it.
    let (repo, home) = fixture("global");
    ok(&run(&repo, &home, &["zconfig", "autostatus", "on"]), "set autostatus");

    let g = global(&home);
    assert!(g.contains("autostatus"), "the setting is not in the global config:\n{g}");
    assert!(g.contains("true"), "the value is not in the global config:\n{g}");
    assert!(
        !repo_config(&repo).contains("autostatus"),
        "the setting leaked into the repository config:\n{}",
        repo_config(&repo)
    );

    // And reading it back reports the value rather than the default.
    let shown = ok(&run(&repo, &home, &["zconfig", "autostatus"]), "show autostatus");
    assert!(shown.contains("on") || shown.contains("true"), "{shown}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn all_off_turns_off_the_gated_switches_and_leaves_the_debounce_alone() {
    // `interval` is the one ungated row: it is a delay, not a loop, and zeroing
    // it would make the daemon react with no debounce at all rather than not
    // react. `all off` must skip it.
    let (repo, home) = fixture("all");
    ok(&run(&repo, &home, &["zconfig", "all", "off"]), "all off");

    let g = global(&home);
    for key in ["autoreconcile", "autobump", "autocrawl", "autostatus", "autohook", "autodups"] {
        assert!(g.contains(key), "`all off` did not write {key}:\n{g}");
    }
    // The gated counts are disabled with 0 …
    assert!(g.contains("statusinterval = 0"), "statusinterval was not disabled:\n{g}");
    assert!(g.contains("watchmru = 0"), "watchmru was not disabled:\n{g}");
    // … and the ungated debounce is untouched.
    assert!(!g.contains("interval = 0\n") || g.contains("statusinterval = 0"), "{g}");
    assert!(
        !g.lines().any(|l| l.trim().starts_with("interval =")),
        "`all off` wrote the ungated debounce:\n{g}"
    );

    // `all on` restores the gated counts to their cadence rather than to 1.
    ok(&run(&repo, &home, &["zconfig", "all", "on"]), "all on");
    let g = global(&home);
    assert!(g.contains("statusinterval = 10"), "`all on` did not restore the cadence:\n{g}");
    assert!(g.contains("watchmru = 512"), "`all on` did not restore the cadence:\n{g}");

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn default_reverts_a_setting_to_unset() {
    let (repo, home) = fixture("default");
    ok(&run(&repo, &home, &["zconfig", "autohook", "on"]), "set");
    assert!(global(&home).contains("autohook"));

    ok(&run(&repo, &home, &["zconfig", "autohook", "default"]), "revert");
    assert!(
        !global(&home).lines().any(|l| l.trim().starts_with("autohook")),
        "`default` left the key set:\n{}",
        global(&home)
    );
    // And the listing stops marking it.
    let out = ok(&run(&repo, &home, &["zconfig"]), "list");
    assert!(
        !marked_rows(&out).iter().any(|l| l.contains("autohook")),
        "`default` left the row marked:\n{out}"
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_value_the_daemon_could_not_parse_is_refused() {
    let (repo, home) = fixture("bad");
    // A count is not a boolean.
    let out = run(&repo, &home, &["zconfig", "statusinterval", "on"]);
    assert!(!out.status.success(), "a count accepted a boolean");
    // A boolean is not a count.
    let out = run(&repo, &home, &["zconfig", "autobump", "7"]);
    assert!(!out.status.success(), "a boolean accepted a count");
    // `all` takes on|off and nothing else.
    let out = run(&repo, &home, &["zconfig", "all", "maybe"]);
    assert!(!out.status.success(), "`all maybe` was accepted");
    // An unknown setting is refused rather than written.
    let out = run(&repo, &home, &["zconfig", "nosuchsetting", "on"]);
    assert!(!out.status.success(), "an unknown setting was accepted");
    assert!(
        !global(&home).contains("nosuchsetting"),
        "a refused setting was still written:\n{}",
        global(&home)
    );

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
