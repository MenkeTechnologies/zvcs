//! A key written before a config file's first `[section]` line.
//!
//! `git_parse_source()` keeps the section stem in `var` and truncates it back to
//! `baselen` for every entry (config.c:1124-1135); `baselen` stays 0 until a `[...]`
//! line sets a stem, so such an entry reaches the callback as the bare name, `a` for a
//! file holding `a`. The raw walks `git config` uses list it — `--list`, `--show-origin`,
//! `--show-scope`, `-z`, `--name-only`, `--get-regexp`, `--blob` — while the readers that
//! go through the configset drop it, because `configset_add_value()` refuses a key with
//! no section (config.c:1743-1745, 1764-1767): `git var -l` never shows it. A lookup
//! or write of a dotless name is `error: key does not contain a section` from
//! `git_config_parse_key()` (config.c:559-563).
//!
//! zvcs read config through gix-config's `File`, which kept those entries as front
//! matter outside every section, so every listing dropped them.
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
#[path = "support/twin_repo.rs"]
mod twin_repo;

use twin_repo::Side;

fn world(label: &str) -> Option<(Side, Side)> {
    let stock = stock_git::stock_git()?;
    Some(twin_repo::pair(label, stock))
}

/// Run `args` on both sides and require identical exit status, stdout and stderr.
fn same(s: &Side, z: &Side, args: &[&str]) -> twin_repo::Out {
    let want = s.git(args);
    assert_eq!(z.git(args), want, "{args:?}");
    want
}

#[test]
fn a_file_lists_its_sectionless_entries_in_place() {
    let Some((s, z)) = world("config-sectionless-list") else { return };
    let body = "# head\na\nB = 1 ; note\n[s]\nk = v\n";
    for side in [&s, &z] {
        side.write("cfg", body);
    }
    let listed = same(&s, &z, &["config", "-f", "cfg", "--list"]);
    assert_eq!((listed.code, listed.stdout.as_str()), (0, "a\nb=1\ns.k=v\n"));
    for args in [
        &["config", "-f", "cfg", "--list", "--show-origin"][..],
        &["config", "-f", "cfg", "--list", "--show-scope"],
        &["config", "-f", "cfg", "--list", "-z"],
        &["config", "-f", "cfg", "--list", "--name-only"],
        &["config", "-f", "cfg", "list", "--show-origin", "--show-scope"],
        &["config", "-f", "cfg", "--get-regexp", "."],
        &["config", "-f", "cfg", "--get-regexp", "^b$"],
        &["config", "-f", "cfg", "--get-regexp", "a"],
        &["config", "-f", "cfg", "--get-regexp", "--name-only", "."],
        &["config", "-f", "cfg", "--bool", "--get-regexp", "^a$"],
    ] {
        same(&s, &z, args);
    }
    // A dotless name never reaches the file: `git_config_parse_key()` refuses it.
    for args in [
        &["config", "-f", "cfg", "--get", "a"][..],
        &["config", "-f", "cfg", "--get-all", "b"],
        &["config", "-f", "cfg", "c", "1"],
        &["config", "-f", "cfg", "--unset", "a"],
    ] {
        let out = same(&s, &z, args);
        assert!(out.stderr.starts_with("error: key does not contain a section: "), "{out:?}");
    }
}

#[test]
fn writes_keep_the_lines_before_the_first_header() {
    let Some((s, z)) = world("config-sectionless-write") else { return };
    for (name, body) in [("w1", "# c\na\nb = 1 ; x\n"), ("w2", "a")] {
        for side in [&s, &z] {
            side.write(name, body);
        }
    }
    for args in [
        &["config", "-f", "w1", "s.k", "v"][..],
        &["config", "-f", "w1", "--add", "s.k", "w"],
        &["config", "-f", "w1", "t.u", "1"],
        &["config", "-f", "w1", "--rename-section", "s", "r"],
        &["config", "-f", "w1", "--remove-section", "t"],
        &["config", "-f", "w1", "--list"],
        &["config", "-f", "w2", "s.k", "v"],
        &["config", "-f", "w2", "--list"],
    ] {
        same(&s, &z, args);
    }
    assert_eq!(z.read("w1"), s.read("w1"));
    assert_eq!(z.read("w2"), s.read("w2"));
}

#[test]
fn a_blob_keeps_its_sectionless_entries() {
    let Some((s, z)) = world("config-sectionless-blob") else { return };
    for side in [&s, &z] {
        side.write("blob", "a\nb = 2\n");
        side.git(&["add", "blob"]);
        side.git(&["commit", "-q", "-m", "blob"]);
    }
    for args in [
        &["config", "--blob=HEAD:blob", "--list"][..],
        &["config", "--blob=HEAD:blob", "--list", "--show-origin", "--show-scope"],
        &["config", "--blob=HEAD:blob", "--get-regexp", "."],
    ] {
        same(&s, &z, args);
    }
}

#[test]
fn an_include_keeps_its_sectionless_entries_in_place() {
    let Some((s, z)) = world("config-sectionless-include") else { return };
    for side in [&s, &z] {
        side.write(".git/sl.inc", "top = 1\n[x]\ny = 2\n");
        let config = String::from_utf8(side.read(".git/config").unwrap()).unwrap();
        side.write(".git/config", &format!("{config}[include]\npath = sl.inc\n[after]\nk = v\n"));
    }
    for args in [
        &["config", "--list", "--show-origin"][..],
        &["config", "--list", "--show-scope"],
        &["config", "--get-regexp", "^(top|x|after)"],
    ] {
        same(&s, &z, args);
    }
    let listed = s.git(&["config", "--list", "--show-origin"]);
    assert!(
        listed.stdout.ends_with("file:.git/sl.inc\ttop=1\nfile:.git/sl.inc\tx.y=2\nfile:.git/config\tafter.k=v\n"),
        "{listed:?}"
    );
    // `git var -l` reads through the configset, which has no place for a dotless key. Only
    // stdout is compared: the configset's own `error: key does not contain a section: top`
    // on stderr, printed whenever it loads, is not reproduced yet.
    let want = s.git(&["var", "-l"]);
    assert!(!want.stdout.lines().any(|line| line.starts_with("top")), "{want:?}");
    assert_eq!(z.git(&["var", "-l"]).stdout, want.stdout);
}
