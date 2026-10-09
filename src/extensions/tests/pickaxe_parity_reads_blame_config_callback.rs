//! `git pickaxe` is `cmd_blame()` under its newer name, so it reads configuration through
//! `blame_config()` and the settings block before it parses an option. zvcs only gave
//! `blame` and `annotate` that, so a bad `core.*` / `blame.*` value reached the usage
//! block (129) instead of dying at 128.
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

#[test]
fn pickaxe_dies_on_bad_config_exactly_as_blame_does() {
    let Some((s, z)) = world("pickaxe-config") else { return };
    for (key, value) in [
        ("core.abbrev", " 1"),
        ("blame.showRoot", "always"),
        ("core.safecrlf", "none"),
        ("core.packedGitLimit", "bogus"),
    ] {
        for args in [&["pickaxe"][..], &["pickaxe", "-h"], &["pickaxe", "--no-color-lines"], &["blame", "a"]] {
            let mut full = vec!["-c".to_owned(), format!("{key}={value}")];
            full.extend(args.iter().map(|a| a.to_string()));
            let argv: Vec<&str> = full.iter().map(String::as_str).collect();
            let want = s.git(&argv);
            assert_eq!(z.git(&argv), want, "{argv:?}");
        }
    }
    let bad = s.git(&["-c", "blame.showRoot=always", "pickaxe"]);
    assert_eq!(bad.code, 128, "{bad:?}");
}
