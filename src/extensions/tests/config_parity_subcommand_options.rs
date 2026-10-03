//! Each `git config <subcommand>` parses its own option table.
//!
//! `cmd_config_list()` … `cmd_config_edit()` (builtin/config.c:1038-1340) build
//! their tables from `CONFIG_LOCATION_OPTIONS`, `CONFIG_DISPLAY_OPTIONS` and
//! `CONFIG_TYPE_OPTIONS` in different combinations, so an option the legacy form
//! takes can be unknown to a subcommand: `set --show-origin`, `unset
//! --show-origin`, `list --comment`, `edit --type=bool` are `unknown option`
//! with that subcommand's block on stderr, exit 129, and nothing is written.
//! Abbreviations resolve against the subcommand's table too, so `--sho` is
//! unknown to `set` and ambiguous to `list`. zvcs handed every option to the
//! legacy parser, which ran `set --show-origin a.b c` as a plain set.
//! Expectations captured from stock git 2.56.0.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn git(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("ZVCS_HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .output()
        .expect("run the binary under test");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().expect("no signal"),
    )
}

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!("zvcs-config-sub-opts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "a.b", "x"]);
    root
}

#[test]
fn an_option_outside_the_subcommand_table_is_unknown() {
    let root = fixture();
    let config = || std::fs::read_to_string(root.join(".git/config")).unwrap();
    let before = config();

    // `-h` prints the block on stdout; a refusal prints the same block on stderr.
    let (set_block, _, code) = git(&root, &["config", "set", "-h"]);
    assert_eq!(code, 0);
    assert!(set_block.starts_with("usage: git config set [<file-option>] [--type=<type>]"));
    assert!(set_block.ends_with(
        "    --[no-]append         add a new line without altering any existing values\n\n"
    ));
    let (unset_block, _, _) = git(&root, &["config", "unset", "-h"]);
    let (list_block, _, _) = git(&root, &["config", "list", "-h"]);
    let (edit_block, _, _) = git(&root, &["config", "edit", "-h"]);
    assert!(!list_block.contains("comment") && list_block.contains("--[no-]show-origin"));

    let cases: [(&[&str], String); 6] = [
        (&["config", "set", "--show-origin", "a.b", "c"], format!("error: unknown option `show-origin'\n{set_block}")),
        (&["config", "unset", "--show-origin", "a.b"], format!("error: unknown option `show-origin'\n{unset_block}")),
        (&["config", "set", "--sho", "a.b", "c"], format!("error: unknown option `sho'\n{set_block}")),
        (&["config", "set", "-z", "a.b", "c"], format!("error: unknown switch `z'\n{set_block}")),
        (&["config", "edit", "--type=bool"], format!("error: unknown option `type=bool'\n{edit_block}")),
        (
            &["config", "list", "--sho"],
            format!("error: ambiguous option: sho (could be --show-scope or --show-names)\n{list_block}"),
        ),
    ];
    for (args, want) in cases {
        assert_eq!(git(&root, args), (String::new(), want, 129), "{args:?}");
    }
    assert_eq!(config(), before);

    // An abbreviation the table does resolve still works.
    assert_eq!(git(&root, &["config", "get", "--show-o", "a.b"]).0, "file:.git/config\tx\n");
    let _ = std::fs::remove_dir_all(&root);
}

/// `get_arg()` reads a clustered value switch's argument from the next word, and
/// `--no-url` / `--no-value` are `OPT_STRING`'s unset sense, clearing the string.
/// zvcs took `-zf <file>` for `-z -f` with `<file>` an operand and refused the
/// two negations as unknown legacy options.
#[test]
fn clustered_values_and_negated_strings() {
    let root = std::env::temp_dir().join(format!("zvcs-config-sub-values-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "a.b", "x"]);

    let (out, err, code) = git(&root, &["config", "get", "-zf", ".git/config", "a.b"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("x\0", "", 0));
    let (out, err, code) = git(&root, &["config", "list", "-zf"]);
    assert_eq!((out.as_str(), err.as_str(), code), ("", "error: switch `f' requires a value\n", 129));
    assert_eq!(git(&root, &["config", "get", "--url", "https://h", "--no-url", "a.b"]).0, "x\n");
    assert_eq!(git(&root, &["config", "get", "--value=y", "--no-value", "a.b"]).0, "x\n");
    let _ = std::fs::remove_dir_all(&root);
}
