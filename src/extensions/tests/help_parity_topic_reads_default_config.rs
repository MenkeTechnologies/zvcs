//! `cmd_help()` reads configuration only once it has a topic:
//! `repo_config(the_repository, git_help_config, NULL)` (builtin/help.c:744) keeps
//! `help.format`, `help.htmlpath` and `man.*` for itself and hands every other
//! value to `git_default_config()`. A value that callback refuses ends `git help
//! <topic>` — in any of its viewers — before a page is looked up, while the
//! listing modes (`-a`, `-g`, `--config`, bare `help`) never read it.
//!
//! zvcs read only the three help keys, so `git -c push.default=0x10 help
//! attributes` went on to open the manual.

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin.rs"]
mod twin;
use twin::Twin;

const REFUSED: &[&str] = &[
    "push.default=0x10",
    "core.ignorecase=bogus",
    "color.advice.reset=bogus",
    "advice.statusHints=bogus",
    "user.useConfigOnly=bogus",
];

#[test]
fn a_topic_is_refused_for_a_value_the_default_callback_refuses() {
    let Some(t) = Twin::new("help-topic-default-config") else { return };
    for key in REFUSED {
        for viewer in ["-m", "-i", "-w"] {
            t.same(&["-c", key, "help", viewer, "attributes"]);
        }
        t.same(&["-c", key, "help", "attributes"]);
        t.same(&["-c", key, "help", "git"]);
    }
}

#[test]
fn it_is_refused_from_a_config_file_too() {
    let Some(t) = Twin::new("help-topic-default-config-file") else { return };
    t.prepare(&["config", "core.ignorecase", "bogus"]);
    t.same(&["help", "attributes"]);
    t.same(&["help", "-a"]);
}

#[test]
fn the_listing_modes_never_read_it() {
    let Some(t) = Twin::new("help-listing-default-config") else { return };
    for key in REFUSED {
        t.same(&["-c", key, "help"]);
        t.same(&["-c", key, "help", "-g"]);
        t.same(&["-c", key, "help", "--config"]);
        t.same(&["-c", key, "help", "--user-interfaces"]);
    }
}

#[test]
fn the_keys_help_keeps_for_itself_are_not_run_through_the_default_callback() {
    let Some(t) = Twin::new("help-topic-own-keys") else { return };
    t.same(&["-c", "help.format=nosuchformat", "help", "attributes"]);
    t.same(&["-c", "help.format=nosuchformat", "-c", "core.ignorecase=bogus", "help", "attributes"]);
}
